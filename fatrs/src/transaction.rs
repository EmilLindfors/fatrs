//! Transaction safety infrastructure for power-loss resilience
//!
//! This module implements a minimal two-phase commit protocol to ensure
//! filesystem consistency even in the event of sudden power loss.
//!
//! # Architecture
//!
//! ## Intent Log
//! - Reserved sectors on disk store pending operations
//! - Each transaction records: operation type, affected sectors, checksums
//! - Log is checked on mount and replayed/rolled back as needed
//!
//! ## Two-Phase Commit Protocol
//! 1. **Write Intent**: Record operation details to intent log
//! 2. **Perform Operation**: Execute the actual disk writes
//! 3. **Clear Intent**: Mark transaction as complete
//!
//! ## Recovery
//! - On mount, check for incomplete transactions
//! - If intent exists, determine if operation completed
//! - Replay or rollback as appropriate
//!
//! # Safety Guarantees
//!
//! - **Atomicity**: Metadata operations either complete fully or not at all
//! - **Consistency**: Filesystem remains valid after power loss
//! - **Durability**: Once committed, changes survive power loss
//!
//! # Use Cases
//! - Medical devices
//! - Automotive systems
//! - Aerospace applications
//! - Any safety-critical embedded system

#![allow(clippy::missing_errors_doc)]
#![allow(clippy::must_use_candidate)]
#![allow(clippy::doc_markdown)]
#![allow(dead_code)]

use crate::error::Error;
use crate::io::{Read, ReadLeExt, Seek, SeekFrom, Write, WriteLeExt};
use core::fmt::Debug;

/// Maximum number of concurrent transactions that can be logged
const MAX_TRANSACTIONS: usize = 4;

/// Size of each transaction log entry in bytes
const TRANSACTION_ENTRY_SIZE: usize = 512;

/// Magic number to identify valid transaction log
const TRANSACTION_MAGIC: u32 = 0x5458_4E46; // "TXNF"

/// Transaction log version
const TRANSACTION_VERSION: u16 = 1;

/// Type of filesystem operation being logged
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TransactionType {
    /// No active transaction (empty log slot)
    None = 0,
    /// FAT entry update (cluster allocation/deallocation)
    FatUpdate = 1,
    /// Directory entry modification
    DirEntryUpdate = 2,
    /// FSInfo sector update (free cluster count, etc.)
    FsInfoUpdate = 3,
    /// File size/timestamp update
    FileMetadataUpdate = 4,
    /// Cluster chain modification (extend/truncate file)
    ClusterChainUpdate = 5,
}

impl TransactionType {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(TransactionType::None),
            1 => Some(TransactionType::FatUpdate),
            2 => Some(TransactionType::DirEntryUpdate),
            3 => Some(TransactionType::FsInfoUpdate),
            4 => Some(TransactionType::FileMetadataUpdate),
            5 => Some(TransactionType::ClusterChainUpdate),
            _ => None,
        }
    }
}

/// Status of a transaction
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum TransactionState {
    /// Transaction slot is empty
    Empty = 0,
    /// Intent written, operation not yet performed
    Pending = 1,
    /// Operation in progress
    InProgress = 2,
    /// Operation completed successfully
    Committed = 3,
}

impl TransactionState {
    fn from_u8(value: u8) -> Option<Self> {
        match value {
            0 => Some(TransactionState::Empty),
            1 => Some(TransactionState::Pending),
            2 => Some(TransactionState::InProgress),
            3 => Some(TransactionState::Committed),
            _ => None,
        }
    }
}

/// A transaction log entry
///
/// Layout (512 bytes):
/// - Magic (4 bytes): 0x54584E46 "TXNF"
/// - Version (2 bytes): Log format version
/// - Type (1 byte): TransactionType
/// - State (1 byte): TransactionState
/// - Sequence (4 bytes): Monotonic counter
/// - Timestamp (8 bytes): Operation timestamp
/// - Sector count (2 bytes): Number of affected sectors
/// - Sectors (up to 64 × 4 bytes = 256 bytes): List of affected sector numbers
/// - Backup data (200 bytes): Original sector data for rollback
/// - CRC32 (4 bytes): Checksum of entry
/// - Reserved (22 bytes): For future use
#[derive(Debug, Clone)]
pub struct TransactionEntry {
    pub magic: u32,
    pub version: u16,
    pub tx_type: TransactionType,
    pub state: TransactionState,
    pub sequence: u32,
    pub timestamp: u64,
    pub affected_sectors: [u32; 64],
    pub sector_count: u16,
    pub backup_data: [u8; 200],
    pub crc32: u32,
}

impl TransactionEntry {
    /// Create a new empty transaction entry
    pub fn new() -> Self {
        Self {
            magic: TRANSACTION_MAGIC,
            version: TRANSACTION_VERSION,
            tx_type: TransactionType::None,
            state: TransactionState::Empty,
            sequence: 0,
            timestamp: 0,
            affected_sectors: [0; 64],
            sector_count: 0,
            backup_data: [0; 200],
            crc32: 0,
        }
    }

    /// Serialize transaction entry to bytes
    pub async fn serialize<W: Write>(&self, writer: &mut W) -> Result<(), W::Error> {
        writer.write_u32_le(self.magic).await?;
        writer.write_u16_le(self.version).await?;
        writer.write_u8(self.tx_type as u8).await?;
        writer.write_u8(self.state as u8).await?;
        writer.write_u32_le(self.sequence).await?;
        writer.write_u64_le(self.timestamp).await?;
        writer.write_u16_le(self.sector_count).await?;

        // Write affected sectors
        for &sector in &self.affected_sectors {
            writer.write_u32_le(sector).await?;
        }

        // Write backup data
        writer.write_all(&self.backup_data).await?;

        // Write CRC
        writer.write_u32_le(self.crc32).await?;

        // Write reserved bytes (padding to 512 bytes)
        let reserved = [0u8; 22];
        writer.write_all(&reserved).await?;

        Ok(())
    }

    /// Deserialize transaction entry from bytes
    pub async fn deserialize<R: Read>(reader: &mut R) -> Result<Self, Error<R::Error>> {
        let magic = reader.read_u32_le().await?;
        let version = reader.read_u16_le().await?;
        let tx_type =
            TransactionType::from_u8(reader.read_u8().await?).ok_or(Error::CorruptedFileSystem)?;
        let state =
            TransactionState::from_u8(reader.read_u8().await?).ok_or(Error::CorruptedFileSystem)?;
        let sequence = reader.read_u32_le().await?;
        let timestamp = reader.read_u64_le().await?;
        let sector_count = reader.read_u16_le().await?;

        // Read affected sectors
        let mut affected_sectors = [0u32; 64];
        for sector in &mut affected_sectors {
            *sector = reader.read_u32_le().await?;
        }

        // Read backup data
        let mut backup_data = [0u8; 200];
        reader.read_exact(&mut backup_data).await?;

        // Read CRC
        let crc32 = reader.read_u32_le().await?;

        // Skip reserved bytes
        let mut reserved = [0u8; 22];
        reader.read_exact(&mut reserved).await?;

        Ok(Self {
            magic,
            version,
            tx_type,
            state,
            sequence,
            timestamp,
            affected_sectors,
            sector_count,
            backup_data,
            crc32,
        })
    }

    /// Calculate CRC32 checksum of entry data using the `crc` crate.
    pub fn calculate_crc32(&self) -> u32 {
        use crc::{CRC_32_ISO_HDLC, Crc};

        const CRC32: Crc<u32> = Crc::<u32>::new(&CRC_32_ISO_HDLC);
        let mut digest = CRC32.digest();

        // Hash magic, version, type, state
        digest.update(&self.magic.to_le_bytes());
        digest.update(&self.version.to_le_bytes());
        digest.update(&[self.tx_type as u8]);
        digest.update(&[self.state as u8]);
        digest.update(&self.sequence.to_le_bytes());
        digest.update(&self.timestamp.to_le_bytes());
        digest.update(&self.sector_count.to_le_bytes());

        // Hash affected sectors
        for &sector in &self.affected_sectors[..self.sector_count as usize] {
            digest.update(&sector.to_le_bytes());
        }

        // Hash backup data
        digest.update(&self.backup_data);

        digest.finalize()
    }

    /// Verify CRC32 checksum
    pub fn verify_crc32(&self) -> bool {
        self.crc32 == self.calculate_crc32()
    }

    /// Check if entry is valid (magic and CRC match)
    pub fn is_valid(&self) -> bool {
        self.magic == TRANSACTION_MAGIC && self.verify_crc32()
    }
}

impl Default for TransactionEntry {
    fn default() -> Self {
        Self::new()
    }
}

/// Transaction log manager
///
/// Manages a circular buffer of transaction entries stored in reserved sectors.
pub struct TransactionLog {
    /// Starting sector of transaction log area
    log_start_sector: u32,
    /// Number of sectors allocated for transaction log
    log_sector_count: u32,
    /// Current transaction sequence number (monotonic counter)
    pub(crate) sequence: u32,
    /// Active transaction entries
    entries: [TransactionEntry; MAX_TRANSACTIONS],
}

/// Transaction log statistics
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Debug, Clone, Copy)]
pub struct TransactionStatistics {
    /// Total number of transaction slots available
    pub total_slots: usize,
    /// Number of currently used transaction slots
    pub used_slots: usize,
    /// Current sequence number (monotonic counter)
    pub sequence_number: u32,
}

impl TransactionLog {
    /// Create a new transaction log
    ///
    /// # Arguments
    /// * `log_start_sector` - First sector of reserved log area
    /// * `log_sector_count` - Number of sectors allocated for log
    pub fn new(log_start_sector: u32, log_sector_count: u32) -> Self {
        Self {
            log_start_sector,
            log_sector_count,
            sequence: 0,
            entries: [
                TransactionEntry::new(),
                TransactionEntry::new(),
                TransactionEntry::new(),
                TransactionEntry::new(),
            ],
        }
    }

    /// Initialize transaction log on disk
    pub async fn initialize<IO: Read + Write + Seek>(
        &mut self,
        disk: &mut IO,
    ) -> Result<(), Error<IO::Error>> {
        // Write empty transaction entries
        for i in 0..MAX_TRANSACTIONS {
            let sector = self.log_start_sector + i as u32;
            disk.seek(SeekFrom::Start(u64::from(sector) * 512)).await?;

            let mut entry = TransactionEntry::new();
            entry.crc32 = entry.calculate_crc32();
            entry.serialize(disk).await?;
        }

        disk.flush().await?;
        Ok(())
    }

    /// Load transaction log from disk
    pub async fn load<IO: Read + Write + Seek>(
        &mut self,
        disk: &mut IO,
    ) -> Result<(), Error<IO::Error>> {
        let mut max_sequence = 0;

        // Read all transaction entries
        for i in 0..MAX_TRANSACTIONS {
            let sector = self.log_start_sector + i as u32;
            disk.seek(SeekFrom::Start(u64::from(sector) * 512)).await?;

            let entry = TransactionEntry::deserialize(disk).await?;

            // Track highest sequence number
            if entry.sequence > max_sequence {
                max_sequence = entry.sequence;
            }

            self.entries[i] = entry;
        }

        // Continue from highest sequence number
        self.sequence = max_sequence.saturating_add(1);

        Ok(())
    }

    /// Begin a new transaction
    ///
    /// Returns the transaction slot index, or None if all slots are full.
    pub fn begin_transaction(
        &mut self,
        tx_type: TransactionType,
        affected_sectors: &[u32],
    ) -> Option<usize> {
        // Find an empty slot
        let slot = self
            .entries
            .iter()
            .position(|e| e.state == TransactionState::Empty)?;

        let entry = &mut self.entries[slot];
        entry.tx_type = tx_type;
        entry.state = TransactionState::Pending;
        entry.sequence = self.sequence;
        entry.timestamp = 0; // TODO: Get actual timestamp from TimeProvider
        entry.sector_count = affected_sectors.len().min(64) as u16;

        for (i, &sector) in affected_sectors.iter().take(64).enumerate() {
            entry.affected_sectors[i] = sector;
        }

        entry.crc32 = entry.calculate_crc32();

        self.sequence = self.sequence.wrapping_add(1);

        Some(slot)
    }

    /// Write transaction intent to disk
    pub async fn write_intent<IO: Read + Write + Seek>(
        &self,
        disk: &mut IO,
        slot: usize,
    ) -> Result<(), Error<IO::Error>> {
        if slot >= MAX_TRANSACTIONS {
            return Err(Error::InvalidInput);
        }

        let sector = self.log_start_sector + slot as u32;
        disk.seek(SeekFrom::Start(u64::from(sector) * 512)).await?;

        self.entries[slot].serialize(disk).await?;
        disk.flush().await?;

        Ok(())
    }

    /// Mark transaction as in progress
    pub fn mark_in_progress(&mut self, slot: usize) {
        if slot < MAX_TRANSACTIONS {
            self.entries[slot].state = TransactionState::InProgress;
            self.entries[slot].crc32 = self.entries[slot].calculate_crc32();
        }
    }

    /// Commit a transaction (mark as complete)
    pub async fn commit<IO: Read + Write + Seek>(
        &mut self,
        disk: &mut IO,
        slot: usize,
    ) -> Result<(), Error<IO::Error>> {
        if slot >= MAX_TRANSACTIONS {
            return Err(Error::InvalidInput);
        }

        // Mark as committed
        self.entries[slot].state = TransactionState::Committed;
        self.entries[slot].crc32 = self.entries[slot].calculate_crc32();

        // Write updated state
        let sector = self.log_start_sector + slot as u32;
        disk.seek(SeekFrom::Start(u64::from(sector) * 512)).await?;
        self.entries[slot].serialize(disk).await?;
        disk.flush().await?;

        Ok(())
    }

    /// Clear a transaction entry (mark as empty)
    pub async fn clear<IO: Read + Write + Seek>(
        &mut self,
        disk: &mut IO,
        slot: usize,
    ) -> Result<(), Error<IO::Error>> {
        if slot >= MAX_TRANSACTIONS {
            return Err(Error::InvalidInput);
        }

        // Create empty entry
        let mut entry = TransactionEntry::new();
        entry.crc32 = entry.calculate_crc32();

        // Write to disk
        let sector = self.log_start_sector + slot as u32;
        disk.seek(SeekFrom::Start(u64::from(sector) * 512)).await?;
        entry.serialize(disk).await?;
        disk.flush().await?;

        // Update in-memory state
        self.entries[slot] = entry;

        Ok(())
    }

    /// Get all pending or in-progress transactions (for recovery)
    pub fn get_incomplete_transactions(&self) -> impl Iterator<Item = (usize, &TransactionEntry)> {
        self.entries.iter().enumerate().filter(|(_, e)| {
            e.is_valid()
                && (e.state == TransactionState::Pending || e.state == TransactionState::InProgress)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_transaction_entry_crc32() {
        let mut entry = TransactionEntry::new();
        entry.tx_type = TransactionType::FatUpdate;
        entry.state = TransactionState::Pending;
        entry.sequence = 42;
        entry.sector_count = 2;
        entry.affected_sectors[0] = 100;
        entry.affected_sectors[1] = 101;

        let crc = entry.calculate_crc32();
        entry.crc32 = crc;

        assert!(entry.verify_crc32());
        assert!(entry.is_valid());
    }

    #[test]
    fn test_transaction_entry_invalid_crc() {
        let mut entry = TransactionEntry::new();
        entry.tx_type = TransactionType::FatUpdate;
        entry.crc32 = 0x1234_5678; // Wrong CRC

        assert!(!entry.verify_crc32());
        assert!(!entry.is_valid());
    }

    #[test]
    fn test_transaction_log_begin() {
        let mut log = TransactionLog::new(100, 10);

        let slot = log.begin_transaction(TransactionType::FatUpdate, &[200, 201, 202]);
        assert!(slot.is_some());

        let slot = slot.unwrap();
        assert_eq!(log.entries[slot].tx_type, TransactionType::FatUpdate);
        assert_eq!(log.entries[slot].state, TransactionState::Pending);
        assert_eq!(log.entries[slot].sector_count, 3);
        assert_eq!(log.entries[slot].affected_sectors[0], 200);
    }
}
