//! Example: Create a FAT image with transaction-safe reserved sectors
//!
//! This example demonstrates how to format a FAT filesystem with reserved
//! sectors for the transaction log, enabling power-loss resilience.
//!
//! Usage: cargo run --example create_txn_safe_image --features transaction-safe

use embedded_fatfs::{format_volume, FormatVolumeOptions, FatType};
use tokio::fs;
use tokio::io::AsyncWriteExt;

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    println!("Creating FAT16 image with transaction-safe reserved sectors...");

    // Create a 10MB image file
    let image_path = "fat16_txn_safe.img";
    let image_size = 10 * 1024 * 1024; // 10 MB

    // Create and initialize the file
    let mut file = fs::File::create(image_path).await?;
    let zeros = vec![0u8; image_size];
    file.write_all(&zeros).await?;
    file.sync_all().await?;
    drop(file);

    // Re-open for formatting
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(image_path)
        .await?;

    // Format with transaction-safe reserved sectors
    let options = FormatVolumeOptions::new()
        .fat_type(FatType::Fat16)
        .volume_label(*b"TXN SAFE   ") // 11 characters, padded with spaces
        .volume_id(0x12345678)
        .with_transaction_log(); // Adds 4 extra reserved sectors for transaction log

    println!("Formatting with transaction log support...");
    println!("  - FAT Type: FAT16");
    println!("  - Reserved Sectors: 5 (1 boot + 4 transaction log)");
    println!("  - Volume Label: TXN SAFE");

    format_volume(&mut embedded_io_adapters::tokio_1::FromTokio::new(file), options).await?;

    println!("✓ Successfully created {}", image_path);
    println!("\nYou can now mount this image with transaction-safe features enabled:");
    println!("  use embedded_fatfs::{{FileSystem, FsOptions}};");
    println!("  let fs = FileSystem::new(disk, FsOptions::new()).await?;");
    println!("\nThe filesystem will automatically recover from power-loss during metadata operations.");

    Ok(())
}
