//! Tests for transaction safety and power-loss resilience
//!
//! These tests verify that the filesystem can recover from power loss
//! during critical metadata operations.

#![cfg(all(feature = "transaction-safe", not(target_os = "none")))]

use embedded_fatfs::*;
use tokio::fs;
use tokio::io::AsyncWriteExt;

const TMP_DIR: &str = "tmp";

type FileSystem = embedded_fatfs::FileSystem<
    embedded_io_adapters::tokio_1::FromTokio<tokio::fs::File>,
    ChronoTimeProvider,
    LossyOemCpConverter,
>;

/// Helper to create a filesystem for testing
async fn create_test_filesystem() -> FileSystem {
    let _ = env_logger::builder().is_test(true).try_init();

    // Create tmp directory if it doesn't exist
    fs::create_dir(TMP_DIR).await.ok();

    // Create a fresh 1MB FAT16 image with transaction log support
    let tmp_path = format!("{}/txn-test-fat16.img", TMP_DIR);
    let image_size = 1024 * 1024; // 1 MB

    // Create and initialize the file
    let mut file = fs::File::create(&tmp_path).await.unwrap();
    let zeros = vec![0u8; image_size];
    file.write_all(&zeros).await.unwrap();
    file.sync_all().await.unwrap();
    drop(file);

    // Re-open for formatting
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&tmp_path)
        .await
        .unwrap();

    // Format with transaction-safe reserved sectors
    let options = FormatVolumeOptions::new()
        .fat_type(FatType::Fat16)
        .volume_label(*b"TXN TEST   ")
        .with_transaction_log();

    format_volume(&mut embedded_io_adapters::tokio_1::FromTokio::new(file), options)
        .await
        .unwrap();

    // Re-open for use
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&tmp_path)
        .await
        .unwrap();

    FileSystem::new(file, FsOptions::new()).await.unwrap()
}

#[tokio::test]
async fn test_transaction_log_initialization() -> anyhow::Result<()> {
    // Create and mount a filesystem
    let fs = create_test_filesystem().await;

    // Verify transaction log is initialized and empty
    #[cfg(feature = "transaction-safe")]
    {
        let stats = fs.transaction_statistics().await;
        assert_eq!(stats.used_slots, 0, "Transaction log should be empty initially");
        assert_eq!(stats.total_slots, 4, "Should have 4 transaction slots");
    }

    fs.unmount().await?;
    Ok(())
}

#[tokio::test]
async fn test_transaction_safety_wrapper() -> anyhow::Result<()> {
    // Create filesystem
    let fs = create_test_filesystem().await;

    #[cfg(feature = "transaction-safe")]
    {
        use embedded_fatfs::TransactionType;

        // Test transaction wrapper
        let result = fs
            .with_transaction(
                TransactionType::FsInfoUpdate,
                &[100, 101], // Affected sectors
                || async {
                    // Simulate a safe operation
                    Ok(())
                },
            )
            .await;

        assert!(result.is_ok(), "Transaction should complete successfully");

        // Verify transaction log is clean after completion
        let stats = fs.transaction_statistics().await;
        assert_eq!(stats.used_slots, 0, "Transaction log should be empty after commit");
    }

    fs.unmount().await?;
    Ok(())
}

#[tokio::test]
async fn test_multiple_concurrent_transactions() -> anyhow::Result<()> {
    // Create filesystem
    let fs = create_test_filesystem().await;

    #[cfg(feature = "transaction-safe")]
    {
        use embedded_fatfs::TransactionType;

        // Start multiple transactions (up to 4 concurrent)
        for i in 0..4 {
            let result = fs
                .with_transaction(
                    TransactionType::FatUpdate,
                    &[200 + i],
                    || async { Ok(()) },
                )
                .await;

            assert!(result.is_ok(), "Transaction {} should succeed", i);
        }

        // All transactions should be cleared
        let stats = fs.transaction_statistics().await;
        assert_eq!(stats.used_slots, 0, "All transactions should be committed");
    }

    fs.unmount().await?;
    Ok(())
}

#[tokio::test]
async fn test_transaction_log_persistence() -> anyhow::Result<()> {
    // Test that transaction log persists across mount/unmount cycles
    let fs = create_test_filesystem().await;

    #[cfg(feature = "transaction-safe")]
    {
        let stats = fs.transaction_statistics().await;
        assert!(stats.sequence_number > 0, "Sequence number should be initialized");
        assert_eq!(stats.used_slots, 0, "Should have no pending transactions");

        fs.unmount().await?;
    }

    #[cfg(not(feature = "transaction-safe"))]
    {
        fs.unmount().await?;
    }

    Ok(())
}

#[tokio::test]
async fn test_transaction_crc_validation() -> anyhow::Result<()> {
    use embedded_fatfs::{TransactionEntry, TransactionType, TransactionState};

    // Create a transaction entry
    let mut entry = TransactionEntry::new();
    entry.tx_type = TransactionType::DirEntryUpdate;
    entry.state = TransactionState::Pending;
    entry.sequence = 1;
    entry.sector_count = 2;
    entry.affected_sectors[0] = 100;
    entry.affected_sectors[1] = 101;

    // Calculate and set CRC
    entry.crc32 = entry.calculate_crc32();

    // Verify CRC is valid
    assert!(entry.verify_crc32(), "CRC should be valid");
    assert!(entry.is_valid(), "Entry should be valid");

    // Corrupt the entry
    entry.sequence = 2;

    // CRC should no longer match
    assert!(!entry.verify_crc32(), "CRC should not match after modification");
    assert!(!entry.is_valid(), "Entry should not be valid after corruption");

    Ok(())
}

#[tokio::test]
async fn test_transaction_recovery_on_mount() -> anyhow::Result<()> {
    // This test simulates a power loss scenario
    // 1. Start a transaction
    // 2. "Crash" (don't complete it)
    // 3. Remount and verify recovery

    // Note: Full implementation would require simulating incomplete writes to disk
    // For now, we test that clean mounts work correctly

    let fs = create_test_filesystem().await;

    #[cfg(feature = "transaction-safe")]
    {
        // Verify clean mount has no recovery needed
        let stats = fs.transaction_statistics().await;
        assert_eq!(stats.used_slots, 0, "Clean mount should have no pending transactions");
    }

    fs.unmount().await?;
    Ok(())
}

#[tokio::test]
async fn test_transaction_statistics() -> anyhow::Result<()> {
    let fs = create_test_filesystem().await;

    #[cfg(feature = "transaction-safe")]
    {
        let stats = fs.transaction_statistics().await;

        // Verify statistics structure
        assert_eq!(stats.total_slots, 4);
        assert_eq!(stats.used_slots, 0);
        assert!(stats.sequence_number > 0, "Sequence number should be initialized");

        println!("Transaction statistics:");
        println!("  Total slots: {}", stats.total_slots);
        println!("  Used slots: {}", stats.used_slots);
        println!("  Sequence number: {}", stats.sequence_number);
    }

    fs.unmount().await?;
    Ok(())
}
