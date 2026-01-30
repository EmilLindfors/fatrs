//! RP2040/RP2350 internal flash block device implementation for fatrs.
//!
//! This module provides a `BlockDevice<512>` implementation for the internal flash
//! memory on Raspberry Pi RP2040 and RP2350 microcontrollers via `embassy-rp`.
//!
//! ## Features
//!
//! - Async-first using embassy-rp Flash driver
//! - Supports both RP2040 (2MB) and RP2350 (up to 16MB) flash
//! - Configurable flash region (offset + size)
//! - Handles flash sector erase (4KB sectors)
//! - DMA-aligned buffers for optimal performance
//!
//! ## Memory Layout Considerations
//!
//! The RP2040/RP2350 executes code from flash via XIP (Execute In Place). When using
//! internal flash for FAT filesystem storage, you must partition your flash:
//!
//! ```text
//! 0x10000000 ┌─────────────────┐
//!            │ Program Code    │  <- Your firmware
//!            │                 │
//! 0x10180000 ├─────────────────┤  <- Example: 1.5MB for code
//!            │ FAT Filesystem  │  <- fatrs storage region
//!            │                 │
//! 0x10200000 └─────────────────┘  <- Example: 512KB for FAT (2MB total)
//! ```
//!
//! Update your `memory.x` linker script accordingly:
//!
//! ```text
//! MEMORY {
//!     FLASH : ORIGIN = 0x10000000, LENGTH = 1536K  /* Program space */
//!     RAM   : ORIGIN = 0x20000000, LENGTH = 512K
//! }
//! ```
//!
//! ## Example Usage
//!
//! ```ignore
//! use embassy_rp::flash::{Async, Flash};
//! use embassy_rp::peripherals::FLASH;
//! use fatrs_block_platform::RpFlash;
//! use fatrs::FileSystem;
//!
//! #[embassy_executor::task]
//! async fn flash_task(flash: FLASH) {
//!     // Create flash driver
//!     let flash = Flash::<_, Async, { 2 * 1024 * 1024 }>::new(flash, Irqs);
//!
//!     // Use last 512KB for filesystem (offset 1.5MB)
//!     let offset = 1536 * 1024;
//!     let size = 512 * 1024;
//!
//!     let rp_flash = RpFlash::new(flash, offset, size);
//!
//!     // Create FAT filesystem
//!     let fs = FileSystem::new(rp_flash, Default::default()).await?;
//!
//!     // Use filesystem
//!     let mut file = fs.root_dir().create_file("data.txt").await?;
//!     file.write_all(b"Hello from flash!").await?;
//! }
//! ```
//!
//! ## Caveats
//!
//! - **Flash Wear**: Flash has limited erase cycles (~10K-100K). Use wear-leveling for
//!   frequently updated data or consider SD card via SPI for high-write applications.
//! - **Sector Erase**: Flash must be erased before writing. This implementation handles
//!   4KB sector alignment automatically.
//! - **XIP Safety**: The RP2040/RP2350 hardware ensures XIP cache coherency, but avoid
//!   erasing sectors containing executable code.

use aligned::Aligned;
use fatrs_block_device::BlockDevice;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;

/// Error types for RP flash operations.
#[derive(Debug, Clone, Copy)]
#[cfg_attr(feature = "defmt-logging", derive(defmt::Format))]
pub enum Error {
    /// Flash read/write/erase operation failed
    Flash,
    /// Attempted to access beyond the configured flash region
    OutOfBounds,
    /// Invalid offset or size (must be sector-aligned for writes)
    InvalidAlignment,
    /// Invalid seek operation
    InvalidSeek,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Error::Flash => write!(f, "Flash operation failed"),
            Error::OutOfBounds => write!(f, "Access beyond flash region"),
            Error::InvalidAlignment => write!(f, "Invalid sector alignment"),
            Error::InvalidSeek => write!(f, "Invalid seek operation"),
        }
    }
}

impl core::error::Error for Error {}

impl embedded_io_async::Error for Error {
    fn kind(&self) -> embedded_io_async::ErrorKind {
        match self {
            Error::Flash => embedded_io_async::ErrorKind::Other,
            Error::OutOfBounds => embedded_io_async::ErrorKind::OutOfMemory,
            Error::InvalidAlignment => embedded_io_async::ErrorKind::InvalidInput,
            Error::InvalidSeek => embedded_io_async::ErrorKind::InvalidInput,
        }
    }
}

/// Block device wrapper for RP2040/RP2350 internal flash.
///
/// Wraps `embassy_rp::flash::Flash` to implement `BlockDevice<512>` for use with fatrs.
///
/// # Type Parameters
///
/// - `F`: The embassy-rp Flash driver type
/// - `ALIGN`: Buffer alignment requirement (typically `aligned::A4` for ARM)
///
/// # Flash Sectors
///
/// RP2040/RP2350 flash has 4KB (4096 byte) erase sectors. When writing, entire sectors
/// must be erased first. This implementation handles sector management internally.
pub struct RpFlash<F, ALIGN> {
    inner: Mutex<CriticalSectionRawMutex, RpFlashInner<F>>,
    _align: core::marker::PhantomData<ALIGN>,
}

struct RpFlashInner<F> {
    flash: F,
    offset: u32,
    size: u32,
    position: u64,
}

impl<F, ALIGN> RpFlash<F, ALIGN>
where
    F: embedded_storage_async::nor_flash::NorFlash,
{
    /// Create a new flash block device.
    ///
    /// # Parameters
    ///
    /// - `flash`: The embassy-rp Flash driver instance
    /// - `offset`: Byte offset into flash where the FAT region starts (must be 4KB-aligned)
    /// - `size`: Size of the FAT region in bytes (must be multiple of 4KB)
    ///
    /// # Panics
    ///
    /// Panics if `offset` or `size` are not 4KB-aligned.
    ///
    /// # Example
    ///
    /// ```ignore
    /// // Use last 512KB of 2MB flash
    /// let rp_flash = RpFlash::new(flash, 1536 * 1024, 512 * 1024);
    /// ```
    pub fn new(flash: F, offset: u32, size: u32) -> Self {
        const SECTOR_SIZE: u32 = 4096;

        assert!(
            offset % SECTOR_SIZE == 0,
            "Flash offset must be 4KB-aligned"
        );
        assert!(
            size % SECTOR_SIZE == 0,
            "Flash size must be multiple of 4KB"
        );

        Self {
            inner: Mutex::new(RpFlashInner {
                flash,
                offset,
                size,
                position: 0,
            }),
            _align: core::marker::PhantomData,
        }
    }

    /// Get the configured offset of the flash region.
    pub async fn offset(&self) -> u32 {
        self.inner.lock().await.offset
    }

    /// Get the configured size of the flash region.
    pub async fn size_bytes(&self) -> u32 {
        self.inner.lock().await.size
    }
}

impl<F, ALIGN> BlockDevice<512> for RpFlash<F, ALIGN>
where
    F: embedded_storage_async::nor_flash::NorFlash,
    ALIGN: aligned::Alignment,
{
    type Error = Error;
    type Align = ALIGN;

    async fn read(
        &self,
        block_address: u32,
        data: &mut [Aligned<ALIGN, [u8; 512]>],
    ) -> Result<(), Self::Error> {
        let mut inner = self.inner.lock().await;

        let byte_offset = block_address as u64 * 512;
        let read_size = (data.len() * 512) as u64;

        // Bounds check
        if byte_offset + read_size > inner.size as u64 {
            return Err(Error::OutOfBounds);
        }

        let flash_address = (inner.offset as u64 + byte_offset) as u32;

        // Read each block
        for (i, block) in data.iter_mut().enumerate() {
            let addr = flash_address + (i as u32 * 512);
            inner.flash.read(addr, &mut block[..])
                .await.map_err(|_| Error::Flash)?;
        }

        Ok(())
    }

    async fn write(
        &mut self,
        block_address: u32,
        data: &[Aligned<ALIGN, [u8; 512]>],
    ) -> Result<(), Self::Error> {
        let mut inner = self.inner.lock().await;

        let byte_offset = block_address as u64 * 512;
        let write_size = (data.len() * 512) as u64;

        // Bounds check
        if byte_offset + write_size > inner.size as u64 {
            return Err(Error::OutOfBounds);
        }

        let flash_address = (inner.offset as u64 + byte_offset) as u32;

        // RP2040/RP2350 flash has 4KB erase sectors but we use 512B blocks
        // We must use read-modify-write to preserve adjacent data in the same sector
        const SECTOR_SIZE: u32 = 4096;
        let start_sector = flash_address / SECTOR_SIZE;
        let end_sector = (flash_address + write_size as u32 + SECTOR_SIZE - 1) / SECTOR_SIZE;

        // Process each 4KB sector that overlaps with the write
        let mut sector_buffer = [0u8; 4096];

        for sector in start_sector..end_sector {
            let sector_addr = sector * SECTOR_SIZE;

            // Step 1: Read the entire 4KB sector into buffer
            inner.flash.read(sector_addr, &mut sector_buffer)
                .await.map_err(|_| Error::Flash)?;

            // Step 2: Modify the relevant bytes in the buffer
            // Calculate which bytes in this sector need to be updated
            let sector_start = sector_addr;
            let sector_end = sector_addr + SECTOR_SIZE;
            let write_start = flash_address;
            let write_end = flash_address + write_size as u32;

            // Find the overlap between this sector and the write range
            let overlap_start = write_start.max(sector_start);
            let overlap_end = write_end.min(sector_end);

            if overlap_start < overlap_end {
                let offset_in_sector = (overlap_start - sector_start) as usize;
                let offset_in_write = (overlap_start - write_start) as usize;
                let overlap_len = (overlap_end - overlap_start) as usize;

                // Copy the new data into the sector buffer
                let mut write_offset = offset_in_write;
                let mut buffer_offset = offset_in_sector;

                while write_offset < offset_in_write + overlap_len {
                    let block_idx = write_offset / 512;
                    let block_offset = write_offset % 512;
                    let to_copy = (512 - block_offset).min(offset_in_write + overlap_len - write_offset);

                    if block_idx < data.len() {
                        sector_buffer[buffer_offset..buffer_offset + to_copy]
                            .copy_from_slice(&data[block_idx][block_offset..block_offset + to_copy]);
                    }

                    write_offset += to_copy;
                    buffer_offset += to_copy;
                }
            }

            // Step 3: Erase the 4KB sector
            inner.flash.erase(sector_addr, sector_addr + SECTOR_SIZE)
                .await.map_err(|_| Error::Flash)?;

            // Step 4: Write the entire 4KB sector back
            inner.flash.write(sector_addr, &sector_buffer)
                .await.map_err(|_| Error::Flash)?;
        }

        Ok(())
    }

    async fn size(&self) -> Result<u64, Self::Error> {
        Ok(self.inner.lock().await.size as u64)
    }

    async fn sync(&mut self) -> Result<(), Self::Error> {
        // Flash writes are synchronous on RP2040/RP2350
        // No explicit sync needed
        Ok(())
    }
}

// Implement embedded-io-async traits for direct use with FileSystem
impl<F, ALIGN> embedded_io_async::ErrorType for RpFlash<F, ALIGN>
where
    F: embedded_storage_async::nor_flash::NorFlash,
{
    type Error = Error;
}

impl<F, ALIGN> embedded_io_async::Read for RpFlash<F, ALIGN>
where
    F: embedded_storage_async::nor_flash::NorFlash,
    ALIGN: aligned::Alignment,
{
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut inner = self.inner.lock().await;

        // Check bounds
        if inner.position >= inner.size as u64 {
            return Ok(0); // EOF
        }

        // Calculate how much we can read
        let remaining = (inner.size as u64 - inner.position) as usize;
        let to_read = buf.len().min(remaining);

        // Read from flash
        let flash_address = (inner.offset as u64 + inner.position) as u32;
        inner.flash.read(flash_address, &mut buf[..to_read])
            .await
            .map_err(|_| Error::Flash)?;

        inner.position += to_read as u64;
        Ok(to_read)
    }
}

impl<F, ALIGN> embedded_io_async::Write for RpFlash<F, ALIGN>
where
    F: embedded_storage_async::nor_flash::NorFlash,
    ALIGN: aligned::Alignment,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut inner = self.inner.lock().await;

        // Check bounds
        if inner.position >= inner.size as u64 {
            return Err(Error::OutOfBounds);
        }

        let remaining = (inner.size as u64 - inner.position) as usize;
        let to_write = buf.len().min(remaining);

        if to_write == 0 {
            return Ok(0);
        }

        let flash_address = (inner.offset as u64 + inner.position) as u32;

        // RP2040/RP2350 flash has 4KB erase sectors
        // We must use read-modify-write to preserve adjacent data
        const SECTOR_SIZE: u32 = 4096;
        let start_sector = flash_address / SECTOR_SIZE;
        let end_sector = (flash_address + to_write as u32 + SECTOR_SIZE - 1) / SECTOR_SIZE;

        let mut sector_buffer = [0u8; 4096];

        for sector in start_sector..end_sector {
            let sector_addr = sector * SECTOR_SIZE;

            // Step 1: Read entire 4KB sector
            inner.flash.read(sector_addr, &mut sector_buffer)
                .await
                .map_err(|_| Error::Flash)?;

            // Step 2: Calculate overlap and modify buffer
            let sector_start = sector_addr;
            let sector_end = sector_addr + SECTOR_SIZE;
            let write_start = flash_address;
            let write_end = flash_address + to_write as u32;

            let overlap_start = write_start.max(sector_start);
            let overlap_end = write_end.min(sector_end);

            if overlap_start < overlap_end {
                let offset_in_sector = (overlap_start - sector_start) as usize;
                let offset_in_write = (overlap_start - write_start) as usize;
                let overlap_len = (overlap_end - overlap_start) as usize;

                sector_buffer[offset_in_sector..offset_in_sector + overlap_len]
                    .copy_from_slice(&buf[offset_in_write..offset_in_write + overlap_len]);
            }

            // Step 3: Erase sector
            inner.flash.erase(sector_addr, sector_addr + SECTOR_SIZE)
                .await
                .map_err(|_| Error::Flash)?;

            // Step 4: Write back entire sector
            inner.flash.write(sector_addr, &sector_buffer)
                .await
                .map_err(|_| Error::Flash)?;
        }

        inner.position += to_write as u64;
        Ok(to_write)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        // Flash writes are synchronous on RP2040/RP2350
        Ok(())
    }
}

impl<F, ALIGN> embedded_io_async::Seek for RpFlash<F, ALIGN>
where
    F: embedded_storage_async::nor_flash::NorFlash,
{
    async fn seek(&mut self, pos: embedded_io_async::SeekFrom) -> Result<u64, Self::Error> {
        let mut inner = self.inner.lock().await;

        let new_pos = match pos {
            embedded_io_async::SeekFrom::Start(offset) => offset as i64,
            embedded_io_async::SeekFrom::Current(offset) => inner.position as i64 + offset,
            embedded_io_async::SeekFrom::End(offset) => inner.size as i64 + offset,
        };

        if new_pos < 0 {
            return Err(Error::InvalidSeek);
        }

        if new_pos > inner.size as i64 {
            return Err(Error::InvalidSeek);
        }

        inner.position = new_pos as u64;
        Ok(inner.position)
    }
}
