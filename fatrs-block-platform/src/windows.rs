//! Windows block device implementation for direct disk access
//!
//! This module provides a `BlockDevice<512>` implementation for Windows that can
//! access physical drives (USB flash drives, SD cards, etc.) directly using Win32 APIs.

#![cfg(windows)]

use aligned::{A4, Aligned};
use anyhow::{Context, Result};
use embedded_io_async::{ErrorType, Read, Seek, SeekFrom, Write};
use fatrs_block_device::BlockDevice;
use std::io;
use std::os::windows::fs::OpenOptionsExt;
use std::path::Path;
use windows::Win32::Storage::FileSystem::{
    FILE_FLAG_NO_BUFFERING, FILE_FLAG_WRITE_THROUGH, FILE_SHARE_READ, FILE_SHARE_WRITE,
    GetDriveTypeW, GetLogicalDrives, GetVolumeInformationW,
};
use windows::Win32::System::IO::DeviceIoControl;

const BLOCK_SIZE: usize = 512;

// Windows drive type constant - from winbase.h
const DRIVE_REMOVABLE: u32 = 2;

// IOCTL codes for volume control
const FSCTL_ALLOW_EXTENDED_DASD_IO: u32 = 0x00090083;

/// Windows physical device wrapper for async block I/O
///
/// Provides direct access to Windows drives using unbuffered I/O.
/// Must be opened with appropriate permissions (may require administrator).
pub struct WindowsDevice {
    #[allow(dead_code)]
    file: tokio::fs::File,
    size: u64,
}

impl WindowsDevice {
    /// Open a Windows drive for direct block access
    ///
    /// # Arguments
    /// * `path` - Device path (e.g., "D:", "\\\\.\\D:", or "\\\\.\\PHYSICALDRIVE1")
    /// * `writable` - Whether to open for write access
    ///
    /// # Examples
    /// ```ignore
    /// // Open logical drive
    /// let dev = WindowsDevice::open("D:", false).await?;
    ///
    /// // Open physical drive (requires admin)
    /// let dev = WindowsDevice::open("\\\\.\\PHYSICALDRIVE1", false).await?;
    /// ```
    pub async fn open(path: impl AsRef<Path>, writable: bool) -> Result<Self> {
        let path = path.as_ref();
        let path_str = path.to_string_lossy();

        // Normalize path: "D:" -> "\\.\\D:"
        let device_path = if path_str.len() == 2 && path_str.ends_with(':') {
            format!(r"\\.\{}:", path_str.chars().next().unwrap())
        } else if !path_str.starts_with(r"\\.\") {
            format!(r"\\.\{}", path_str)
        } else {
            path_str.to_string()
        };

        // Open with unbuffered I/O and write-through for consistency
        let file = tokio::task::spawn_blocking({
            let device_path = device_path.clone();
            move || {
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(writable)
                    .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
                    .custom_flags(FILE_FLAG_NO_BUFFERING.0 | FILE_FLAG_WRITE_THROUGH.0)
                    .open(&device_path)
            }
        })
        .await
        .context("Failed to spawn blocking task")?
        .with_context(|| format!("Failed to open device: {}", device_path))?;

        // Get device size
        let size = file
            .metadata()
            .context("Failed to get device metadata")?
            .len();

        let file = tokio::fs::File::from_std(file);

        Ok(Self { file, size })
    }

    /// Get the size of the device in bytes
    pub fn size(&self) -> u64 {
        self.size
    }
}

/// Async I/O wrapper for WindowsDevice
///
/// Wraps blocking Win32 I/O operations in tokio's blocking thread pool
pub struct AsyncWindowsDevice {
    inner: std::sync::Arc<std::sync::Mutex<std::fs::File>>,
    size: u64,
    position: std::sync::Arc<std::sync::Mutex<u64>>,
}

impl AsyncWindowsDevice {
    /// Open a Windows drive for direct block access
    pub async fn open(path: impl AsRef<Path>, _writable: bool) -> Result<Self> {
        let path = path.as_ref();
        let path_str = path.to_string_lossy();

        // Normalize path: "D:" -> "\\.\\D:"
        let device_path = if path_str.len() == 2 && path_str.ends_with(':') {
            format!(r"\\.\{}:", path_str.chars().next().unwrap())
        } else if !path_str.starts_with(r"\\.\") {
            format!(r"\\.\{}", path_str)
        } else {
            path_str.to_string()
        };

        // Open the device - use buffered I/O for simplicity
        // FILE_FLAG_NO_BUFFERING requires sector-aligned access which adds complexity
        let file = tokio::task::spawn_blocking({
            let device_path = device_path.clone();
            move || -> Result<std::fs::File> {
                use std::os::windows::io::AsRawHandle;

                // For mounted volumes, always open read-only to avoid lock conflicts
                // Windows will lock the volume if we open with write access
                let file = std::fs::OpenOptions::new()
                    .read(true)
                    .write(false)  // Force read-only for mounted volumes
                    .share_mode(FILE_SHARE_READ.0 | FILE_SHARE_WRITE.0)
                    .open(&device_path)
                    .with_context(|| {
                        format!(
                            "Failed to open device: {}\n\
                            \n\
                            Note: Accessing raw device handles (\\\\.\\ paths) requires administrator privileges.\n\
                            Please run this command as administrator, or try:\n\
                            1. Right-click Command Prompt and 'Run as Administrator'\n\
                            2. Then run: cargo run --release --bin fatrs -- flash info {}",
                            device_path,
                            device_path.trim_start_matches(r"\\.\")
                        )
                    })?;

                // Use FSCTL_ALLOW_EXTENDED_DASD_IO to allow reading from mounted volumes
                // This signals the file system driver not to perform I/O boundary checks
                // and allows access to all sectors including hidden/boundary sectors
                unsafe {
                    let handle = windows::Win32::Foundation::HANDLE(file.as_raw_handle());
                    let mut bytes_returned = 0u32;

                    let _result = DeviceIoControl(
                        handle,
                        FSCTL_ALLOW_EXTENDED_DASD_IO,
                        None,
                        0,
                        None,
                        0,
                        Some(&mut bytes_returned),
                        None,
                    );
                    // Ignore result - if it fails, we'll try to read anyway
                }

                Ok(file)
            }
        })
        .await
        .context("Failed to spawn blocking task")??;

        // Set a reasonable default size - will be determined properly when reading FAT boot sector
        let size = u64::MAX;

        Ok(Self {
            inner: std::sync::Arc::new(std::sync::Mutex::new(file)),
            size,
            position: std::sync::Arc::new(std::sync::Mutex::new(0)),
        })
    }

    /// Get the size of the device in bytes
    pub fn size(&self) -> u64 {
        self.size
    }
}

impl Clone for AsyncWindowsDevice {
    fn clone(&self) -> Self {
        Self {
            inner: std::sync::Arc::clone(&self.inner),
            size: self.size,
            position: std::sync::Arc::clone(&self.position),
        }
    }
}

impl ErrorType for AsyncWindowsDevice {
    type Error = io::Error;
}

impl Read for AsyncWindowsDevice {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let inner = self.inner.clone();
        let len = buf.len();
        let position = self.position.clone();

        let result = tokio::task::spawn_blocking(move || {
            let mut file = inner.lock().unwrap();
            let mut temp_buf = vec![0u8; len];

            let n = std::io::Read::read(&mut *file, &mut temp_buf).map_err(|e| {
                // Error 483 means the device is locked by Windows
                if e.raw_os_error() == Some(483) {
                    io::Error::other(
                        "Device is locked by Windows. Make sure you're running as Administrator.",
                    )
                } else {
                    e
                }
            })?;

            // Update position
            *position.lock().unwrap() += n as u64;

            Ok::<(Vec<u8>, usize), io::Error>((temp_buf, n))
        })
        .await
        .map_err(io::Error::other)??;

        let (temp_buf, n) = result;
        buf[..n].copy_from_slice(&temp_buf[..n]);
        Ok(n)
    }
}

impl Write for AsyncWindowsDevice {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let inner = self.inner.clone();
        let data = buf.to_vec();

        tokio::task::spawn_blocking(move || {
            let mut file = inner.lock().unwrap();
            std::io::Write::write(&mut *file, &data)
        })
        .await
        .map_err(io::Error::other)?
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        let inner = self.inner.clone();

        tokio::task::spawn_blocking(move || {
            let mut file = inner.lock().unwrap();
            std::io::Write::flush(&mut *file)
        })
        .await
        .map_err(io::Error::other)?
    }
}

impl Seek for AsyncWindowsDevice {
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        let inner = self.inner.clone();
        let position = self.position.clone();

        let std_pos = match pos {
            SeekFrom::Start(n) => std::io::SeekFrom::Start(n),
            SeekFrom::End(n) => std::io::SeekFrom::End(n),
            SeekFrom::Current(n) => std::io::SeekFrom::Current(n),
        };

        tokio::task::spawn_blocking(move || {
            let mut file = inner.lock().unwrap();
            let new_pos = std::io::Seek::seek(&mut *file, std_pos)?;
            *position.lock().unwrap() = new_pos;
            Ok::<u64, io::Error>(new_pos)
        })
        .await
        .map_err(io::Error::other)?
    }
}

impl BlockDevice<BLOCK_SIZE> for AsyncWindowsDevice {
    type Error = io::Error;
    type Align = A4;

    async fn read(
        &mut self,
        block_address: u32,
        data: &mut [Aligned<Self::Align, [u8; BLOCK_SIZE]>],
    ) -> Result<(), Self::Error> {
        self.seek(SeekFrom::Start((block_address as u64) * BLOCK_SIZE as u64))
            .await?;

        for block in data {
            let mut offset = 0;
            while offset < BLOCK_SIZE {
                let n = Read::read(self, &mut block[offset..]).await?;
                if n == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "Unexpected EOF",
                    ));
                }
                offset += n;
            }
        }
        Ok(())
    }

    async fn write(
        &mut self,
        block_address: u32,
        data: &[Aligned<Self::Align, [u8; BLOCK_SIZE]>],
    ) -> Result<(), Self::Error> {
        self.seek(SeekFrom::Start((block_address as u64) * BLOCK_SIZE as u64))
            .await?;

        for block in data {
            let mut offset = 0;
            while offset < BLOCK_SIZE {
                let n = Write::write(self, &block[offset..]).await?;
                if n == 0 {
                    return Err(io::Error::new(io::ErrorKind::WriteZero, "Write returned 0"));
                }
                offset += n;
            }
        }
        Ok(())
    }

    async fn size(&mut self) -> Result<u64, Self::Error> {
        Ok(AsyncWindowsDevice::size(self))
    }
}

/// Information about a removable drive
#[derive(Debug, Clone)]
pub struct DriveInfo {
    /// Drive letter (e.g., "D:")
    pub letter: String,
    /// Device path (e.g., "\\\\.\\D:")
    pub device_path: String,
    /// Physical drive number if detected (e.g., 1 for \\.\PHYSICALDRIVE1)
    pub physical_drive: Option<u32>,
    /// Size in bytes (0 if unavailable)
    pub size: u64,
    /// Volume label (if available)
    pub label: Option<String>,
}

/// List all removable drives (USB flash drives, SD cards, etc.)
pub async fn list_removable_drives() -> Result<Vec<DriveInfo>> {
    tokio::task::spawn_blocking(|| {
        let mut drives = Vec::new();
        let drive_mask = unsafe { GetLogicalDrives() };

        for i in 0..26 {
            if drive_mask & (1 << i) != 0 {
                let letter = (b'A' + i) as char;
                let drive_path = format!("{}:\\", letter);

                // Convert to wide string for Windows API
                let wide_path: Vec<u16> = drive_path.encode_utf16().chain(Some(0)).collect();

                let drive_type =
                    unsafe { GetDriveTypeW(windows::core::PCWSTR(wide_path.as_ptr())) };

                if drive_type == DRIVE_REMOVABLE {
                    let letter_only = format!("{}:", letter);
                    let device_path = format!(r"\\.\{}:", letter);

                    // Try to get volume information
                    let mut volume_name = vec![0u16; 261];
                    let mut serial_number = 0u32;
                    let mut max_component_length = 0u32;
                    let mut file_system_flags = 0u32;
                    let mut file_system_name = vec![0u16; 261];

                    let label = unsafe {
                        GetVolumeInformationW(
                            windows::core::PCWSTR(wide_path.as_ptr()),
                            Some(&mut volume_name),
                            Some(&mut serial_number),
                            Some(&mut max_component_length),
                            Some(&mut file_system_flags),
                            Some(&mut file_system_name),
                        )
                        .ok()
                        .and_then(|_| {
                            let end = volume_name.iter().position(|&c| c == 0).unwrap_or(0);
                            if end > 0 {
                                Some(String::from_utf16_lossy(&volume_name[..end]))
                            } else {
                                None
                            }
                        })
                    };

                    // Try to get size
                    let size = std::fs::metadata(&drive_path)
                        .ok()
                        .map(|m| m.len())
                        .unwrap_or(0);

                    drives.push(DriveInfo {
                        letter: letter_only,
                        device_path,
                        physical_drive: None, // TODO: Detect physical drive mapping
                        size,
                        label,
                    });
                }
            }
        }

        Ok(drives)
    })
    .await
    .context("Failed to spawn blocking task")?
}
