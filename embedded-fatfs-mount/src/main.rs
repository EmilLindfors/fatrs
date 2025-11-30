//! embedded-fatfs-mount CLI tool
//!
//! Mount FAT filesystem images with transaction-safe support using FUSE.
//!
//! # Examples
//!
//! ```bash
//! # Mount a FAT image
//! embedded-fatfs-mount /path/to/image.img /mnt/point
//!
//! # Mount with transaction safety enabled
//! embedded-fatfs-mount /path/to/image.img /mnt/point --transaction-safe
//!
//! # Unmount
//! fusermount -u /mnt/point  # Linux
//! umount /mnt/point         # macOS
//! ```

use std::path::PathBuf;

use clap::Parser;

#[cfg(unix)]
use anyhow::{Context, Result};
#[cfg(unix)]
use log::{error, info};

#[cfg(unix)]
use embedded_fatfs_mount::FuseAdapter;

/// Command-line arguments
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Path to FAT filesystem image
    #[arg(value_name = "IMAGE")]
    image: PathBuf,

    /// Mount point directory
    #[arg(value_name = "MOUNTPOINT")]
    mountpoint: PathBuf,

    /// Enable transaction-safe mode (requires image formatted with transaction log)
    #[arg(short, long)]
    transaction_safe: bool,

    /// Enable verbose logging
    #[arg(short, long)]
    verbose: bool,

    /// Mount as read-only
    #[arg(short, long)]
    read_only: bool,

    /// Allow other users to access the mount
    #[arg(long)]
    allow_other: bool,
}

#[cfg(unix)]
#[tokio::main]
async fn main() -> Result<()> {
    let args = Args::parse();

    // Setup logging
    if args.verbose {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("debug")).init();
    } else {
        env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info")).init();
    }

    info!("embedded-fatfs-mount v{}", env!("CARGO_PKG_VERSION"));
    info!("Mounting {} to {}", args.image.display(), args.mountpoint.display());

    if args.transaction_safe {
        info!("Transaction safety: ENABLED ✓");
    }

    // Verify image exists
    if !args.image.exists() {
        anyhow::bail!("Image file does not exist: {}", args.image.display());
    }

    // Verify mount point exists and is a directory
    if !args.mountpoint.exists() {
        anyhow::bail!("Mount point does not exist: {}", args.mountpoint.display());
    }
    if !args.mountpoint.is_dir() {
        anyhow::bail!("Mount point is not a directory: {}", args.mountpoint.display());
    }

    // Open the FAT image
    let file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(!args.read_only)
        .open(&args.image)
        .await
        .with_context(|| format!("Failed to open image: {}", args.image.display()))?;

    info!("Opened image file");

    // Create filesystem
    let fs = embedded_fatfs::FileSystem::new(
        embedded_io_adapters::tokio_1::FromTokio::new(file),
        embedded_fatfs::FsOptions::new(),
    )
    .await
    .context("Failed to mount FAT filesystem")?;

    info!("FAT filesystem mounted successfully");
    info!("  FAT type: {:?}", fs.fat_type());
    info!("  Volume label: {}", String::from_utf8_lossy(fs.volume_label_as_bytes()));

    #[cfg(feature = "transaction-safe")]
    if args.transaction_safe {
        let stats = fs.transaction_statistics().await;
        info!("  Transaction log: {} slots available", stats.total_slots);
    }

    // Create FUSE adapter
    let fuse_fs = FuseAdapter::new(fs);

    info!("Starting FUSE mount...");

    // Build mount options
    let mut options = vec![
        fuser::MountOption::FSName("embedded-fatfs".to_string()),
        fuser::MountOption::Subtype("fat".to_string()),
    ];

    if args.read_only {
        options.push(fuser::MountOption::RO);
    }

    if args.allow_other {
        options.push(fuser::MountOption::AllowOther);
    }

    // Mount the filesystem
    info!("Filesystem mounted at {}", args.mountpoint.display());
    info!("Press Ctrl+C to unmount");

    match fuser::mount2(fuse_fs, &args.mountpoint, &options) {
        Ok(()) => {
            info!("Filesystem unmounted successfully");
            Ok(())
        }
        Err(e) => {
            error!("Mount failed: {}", e);
            Err(e.into())
        }
    }
}

#[cfg(not(unix))]
fn main() {
    eprintln!("Error: This tool currently only supports Unix-like systems (Linux, macOS, BSD)");
    eprintln!("Windows support via WinFsp is planned for a future release.");
    std::process::exit(1);
}
