//! Linux block device implementation
//!
//! Provides direct access to block devices (e.g., /dev/sdb, /dev/mmcblk0) on Linux.

use aligned::{A4, Aligned};
use embedded_io_async::{ErrorType, Read, Seek, SeekFrom, Write};
use fatrs_block_device::BlockDevice;
use std::io;
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

const BLOCK_SIZE: usize = 512;

/// Linux block device wrapper for async block I/O
///
/// Provides direct access to Linux block devices like /dev/sdb, /dev/mmcblk0, etc.
pub struct LinuxBlockDevice {
    inner: std::sync::Arc<std::sync::Mutex<std::fs::File>>,
    size: u64,
    position: std::sync::Arc<std::sync::Mutex<u64>>,
}

impl LinuxBlockDevice {
    /// Open a Linux block device for direct access
    ///
    /// # Arguments
    /// * `path` - Device path (e.g., "/dev/sdb", "/dev/mmcblk0")
    /// * `writable` - Whether to open for write access
    ///
    /// # Examples
    /// ```ignore
    /// let dev = LinuxBlockDevice::open("/dev/sdb", false).await?;
    /// ```
    pub async fn open(path: impl AsRef<Path>, writable: bool) -> io::Result<Self> {
        let path = path.as_ref();

        // Open with O_DIRECT for unbuffered I/O (requires proper alignment)
        let file = tokio::task::spawn_blocking({
            let path = path.to_owned();
            move || {
                std::fs::OpenOptions::new()
                    .read(true)
                    .write(writable)
                    .custom_flags(libc::O_DIRECT)
                    .open(&path)
            }
        })
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))??;

        // Get device size using BLKGETSIZE64 ioctl
        let size = tokio::task::spawn_blocking({
            let file = file.try_clone()?;
            move || {
                use std::os::unix::io::AsRawFd;
                let mut size: u64 = 0;
                unsafe {
                    // BLKGETSIZE64 = 0x80081272
                    if libc::ioctl(file.as_raw_fd(), 0x80081272, &mut size) < 0 {
                        return Err(io::Error::last_os_error());
                    }
                }
                Ok::<u64, io::Error>(size)
            }
        })
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))??;

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

impl Clone for LinuxBlockDevice {
    fn clone(&self) -> Self {
        Self {
            inner: std::sync::Arc::clone(&self.inner),
            size: self.size,
            position: std::sync::Arc::clone(&self.position),
        }
    }
}

impl ErrorType for LinuxBlockDevice {
    type Error = io::Error;
}

impl Read for LinuxBlockDevice {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let inner = self.inner.clone();
        let len = buf.len();
        let position = self.position.clone();

        let result = tokio::task::spawn_blocking(move || {
            let mut file = inner.lock().unwrap();
            let mut temp_buf = vec![0u8; len];

            let n = std::io::Read::read(&mut *file, &mut temp_buf)?;

            // Update position
            *position.lock().unwrap() += n as u64;

            Ok::<(Vec<u8>, usize), io::Error>((temp_buf, n))
        })
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))??;

        let (temp_buf, n) = result;
        buf[..n].copy_from_slice(&temp_buf[..n]);
        Ok(n)
    }
}

impl Write for LinuxBlockDevice {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let inner = self.inner.clone();
        let data = buf.to_vec();

        tokio::task::spawn_blocking(move || {
            let mut file = inner.lock().unwrap();
            std::io::Write::write(&mut *file, &data)
        })
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        let inner = self.inner.clone();

        tokio::task::spawn_blocking(move || {
            let mut file = inner.lock().unwrap();
            std::io::Write::flush(&mut *file)
        })
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?
    }
}

impl Seek for LinuxBlockDevice {
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
        .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?
    }
}

impl BlockDevice<BLOCK_SIZE> for LinuxBlockDevice {
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
        Ok(LinuxBlockDevice::size(self))
    }
}

/// Information about a block device
#[derive(Debug, Clone)]
pub struct BlockDeviceInfo {
    /// Device path (e.g., "/dev/sdb")
    pub path: String,
    /// Device name (e.g., "sdb")
    pub name: String,
    /// Size in bytes
    pub size: u64,
    /// Whether it's a removable device
    pub removable: bool,
    /// Model name if available
    pub model: Option<String>,
}

/// List all block devices on Linux
pub async fn list_block_devices() -> io::Result<Vec<BlockDeviceInfo>> {
    tokio::task::spawn_blocking(|| {
        let mut devices = Vec::new();

        // Read /sys/block to find all block devices
        for entry in std::fs::read_dir("/sys/block")? {
            let entry = entry?;
            let name = entry.file_name().to_string_lossy().to_string();

            // Skip loop devices and other virtual devices
            if name.starts_with("loop") || name.starts_with("ram") {
                continue;
            }

            let path = format!("/dev/{}", name);
            let sys_path = entry.path();

            // Check if removable
            let removable = std::fs::read_to_string(sys_path.join("removable"))
                .ok()
                .and_then(|s| s.trim().parse::<u8>().ok())
                .map(|v| v == 1)
                .unwrap_or(false);

            // Get size
            let size = std::fs::read_to_string(sys_path.join("size"))
                .ok()
                .and_then(|s| s.trim().parse::<u64>().ok())
                .map(|sectors| sectors * 512)
                .unwrap_or(0);

            // Get model
            let model = std::fs::read_to_string(sys_path.join("device/model"))
                .ok()
                .map(|s| s.trim().to_string());

            devices.push(BlockDeviceInfo {
                path,
                name,
                size,
                removable,
                model,
            });
        }

        Ok(devices)
    })
    .await
    .map_err(|e| io::Error::new(io::ErrorKind::Other, e))?
}
