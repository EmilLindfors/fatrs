//! macOS disk device implementation
//!
//! Provides direct access to disk devices (e.g., /dev/disk2) on macOS.
//!
//! Note: This is a stub implementation. Full macOS support coming soon.

use aligned::{A4, Aligned};
use embedded_io_async::{ErrorType, Read, Seek, SeekFrom, Write};
use fatrs_block_device::BlockDevice;
use std::io;
use std::path::Path;

const BLOCK_SIZE: usize = 512;

/// macOS disk device wrapper for async block I/O
///
/// Provides direct access to macOS disk devices like /dev/disk2, /dev/rdisk2, etc.
pub struct MacOSBlockDevice {
    inner: std::sync::Arc<std::sync::Mutex<std::fs::File>>,
    size: u64,
    position: std::sync::Arc<std::sync::Mutex<u64>>,
}

impl MacOSBlockDevice {
    /// Open a macOS disk device for direct access
    ///
    /// # Arguments
    /// * `path` - Device path (e.g., "/dev/disk2", "/dev/rdisk2")
    /// * `writable` - Whether to open for write access
    ///
    /// # Examples
    /// ```ignore
    /// let dev = MacOSBlockDevice::open("/dev/rdisk2", false).await?;
    /// ```
    pub async fn open(path: impl AsRef<Path>, _writable: bool) -> io::Result<Self> {
        let _path = path.as_ref();
        // TODO: Implement macOS disk access using diskutil and raw device access
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "macOS disk access not yet implemented. Use Linux implementation as a template.",
        ))
    }

    /// Get the size of the device in bytes
    pub fn size(&self) -> u64 {
        self.size
    }
}

// TODO: Implement Clone, ErrorType, Read, Write, Seek, BlockDevice traits

/// Information about a disk device
#[derive(Debug, Clone)]
pub struct DiskInfo {
    /// Device path (e.g., "/dev/disk2")
    pub path: String,
    /// Device name (e.g., "disk2")
    pub name: String,
    /// Size in bytes
    pub size: u64,
    /// Whether it's a removable device
    pub removable: bool,
    /// Volume name if available
    pub volume_name: Option<String>,
}

/// List all disk devices on macOS
pub async fn list_disks() -> io::Result<Vec<DiskInfo>> {
    // TODO: Implement using diskutil list or system APIs
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "macOS disk enumeration not yet implemented",
    ))
}
