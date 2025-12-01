//! Embassy-compatible integration tests
//!
//! These tests use embedded-io-async traits to ensure fatrs works correctly
//! with Embassy and other embedded async runtimes. We use tokio as the test
//! executor but the patterns here apply directly to Embassy.

use std::fs::File;
use std::io::{Read as StdRead, Seek as StdSeek, Write as StdWrite};
use std::sync::{Arc, Mutex};

use embedded_io_async::{ErrorType, Read, Seek, SeekFrom, Write};
use fatrs::{FileAttributes, FileSystem, FsOptions};

/// Embassy-compatible block device wrapper
///
/// This implements embedded-io-async traits around std::fs::File,
/// mimicking how an SD card driver would work in Embassy.
#[derive(Clone)]
struct EmbassyBlockDevice {
    inner: Arc<Mutex<File>>,
}

impl EmbassyBlockDevice {
    fn new(file: File) -> Self {
        Self {
            inner: Arc::new(Mutex::new(file)),
        }
    }
}

impl ErrorType for EmbassyBlockDevice {
    type Error = std::io::Error;
}

impl Read for EmbassyBlockDevice {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let mut file = self.inner.lock().unwrap();
        file.read(buf)
    }
}

impl Write for EmbassyBlockDevice {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let mut file = self.inner.lock().unwrap();
        file.write(buf)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        let mut file = self.inner.lock().unwrap();
        file.flush()
    }
}

impl Seek for EmbassyBlockDevice {
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        let mut file = self.inner.lock().unwrap();
        let std_pos = match pos {
            SeekFrom::Start(n) => std::io::SeekFrom::Start(n),
            SeekFrom::End(n) => std::io::SeekFrom::End(n),
            SeekFrom::Current(n) => std::io::SeekFrom::Current(n),
        };
        file.seek(std_pos)
    }
}

fn create_test_image(path: &str, size_mb: u32) -> std::io::Result<()> {
    use std::process::Command;

    // Ensure target directory exists
    let _ = std::fs::create_dir_all("target");

    // Create empty file
    let file = File::create(path)?;
    file.set_len((size_mb as u64) * 1024 * 1024)?;
    drop(file);

    // Format as FAT32 using mkfs.fat if available, otherwise use our own formatter
    let output = Command::new("mkfs.fat")
        .args(["-F", "32", "-n", "TEST", path])
        .output();

    if output.is_err() || !output.as_ref().unwrap().status.success() {
        // Fall back to internal formatting
        let file = File::options().read(true).write(true).open(path)?;
        let mut device = EmbassyBlockDevice::new(file);

        futures::executor::block_on(async {
            let options = fatrs::FormatVolumeOptions::new()
                .fat_type(fatrs::FatType::Fat32)
                .volume_label(*b"TEST       ");
            fatrs::format_volume(&mut device, options).await.unwrap();
        });
    }

    Ok(())
}

fn cleanup_test_image(path: &str) {
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn test_mount_filesystem() {
    let path = "target/test_mount.img";
    create_test_image(path, 10).unwrap();

    let file = File::options().read(true).write(true).open(path).unwrap();
    let device = EmbassyBlockDevice::new(file);

    let fs = FileSystem::new(device, FsOptions::new()).await.unwrap();

    let stats = fs.stats().await.unwrap();
    assert!(stats.total_clusters() > 0);

    cleanup_test_image(path);
}

#[tokio::test]
async fn test_create_and_read_file() {
    let path = "target/test_create_file.img";
    create_test_image(path, 10).unwrap();

    let file = File::options().read(true).write(true).open(path).unwrap();
    let device = EmbassyBlockDevice::new(file);

    let fs = FileSystem::new(device, FsOptions::new()).await.unwrap();
    let root = fs.root_dir();

    // Create a file
    let test_data = b"Hello, Embassy!";
    let mut file = root.create_file("test.txt").await.unwrap();
    file.write_all(test_data).await.unwrap();
    file.flush().await.unwrap();
    drop(file);

    // Read it back
    let mut file = root.open_file("test.txt").await.unwrap();
    let mut buf = vec![0u8; test_data.len()];
    file.read_exact(&mut buf).await.unwrap();

    assert_eq!(&buf, test_data);

    cleanup_test_image(path);
}

#[tokio::test]
async fn test_write_large_file() {
    let path = "target/test_large_file.img";
    create_test_image(path, 20).unwrap();

    let file = File::options().read(true).write(true).open(path).unwrap();
    let device = EmbassyBlockDevice::new(file);

    let fs = FileSystem::new(device, FsOptions::new()).await.unwrap();
    let root = fs.root_dir();

    // Write 1MB of data
    let chunk = vec![0xAA; 64 * 1024]; // 64KB chunks
    let mut file = root.create_file("large.bin").await.unwrap();

    for _ in 0..16 {
        // 16 * 64KB = 1MB
        file.write_all(&chunk).await.unwrap();
    }
    file.flush().await.unwrap();
    drop(file);

    // Verify file size by seeking to end
    let mut file = root.open_file("large.bin").await.unwrap();
    let size = file.seek(SeekFrom::End(0)).await.unwrap();
    assert_eq!(size, 1024 * 1024);

    cleanup_test_image(path);
}

#[tokio::test]
async fn test_directory_operations() {
    let path = "target/test_dirs.img";
    create_test_image(path, 10).unwrap();

    let file = File::options().read(true).write(true).open(path).unwrap();
    let device = EmbassyBlockDevice::new(file);

    let fs = FileSystem::new(device, FsOptions::new()).await.unwrap();
    let root = fs.root_dir();

    // Create nested directories
    root.create_dir("subdir1").await.unwrap();
    let subdir1 = root.open_dir("subdir1").await.unwrap();
    subdir1.create_dir("subdir2").await.unwrap();

    // Create file in nested directory
    let subdir2 = subdir1.open_dir("subdir2").await.unwrap();
    let mut file = subdir2.create_file("nested.txt").await.unwrap();
    file.write_all(b"nested content").await.unwrap();
    file.flush().await.unwrap();
    drop(file);

    // Read it back via path
    let mut file = root.open_file("subdir1/subdir2/nested.txt").await.unwrap();
    let size = file.seek(SeekFrom::End(0)).await.unwrap();
    assert_eq!(size, 14);

    cleanup_test_image(path);
}

#[tokio::test]
async fn test_file_seek() {
    let path = "target/test_seek.img";
    create_test_image(path, 10).unwrap();

    let file = File::options().read(true).write(true).open(path).unwrap();
    let device = EmbassyBlockDevice::new(file);

    let fs = FileSystem::new(device, FsOptions::new()).await.unwrap();
    let root = fs.root_dir();

    // Create file with known content
    let test_data: Vec<u8> = (0u16..256).map(|x| x as u8).collect();
    let mut file = root.create_file("seek_test.bin").await.unwrap();
    file.write_all(&test_data).await.unwrap();
    file.flush().await.unwrap();
    drop(file);

    // Test seeking
    let mut file = root.open_file("seek_test.bin").await.unwrap();

    // Seek to middle
    file.seek(SeekFrom::Start(128)).await.unwrap();
    let mut buf = [0u8; 1];
    file.read_exact(&mut buf).await.unwrap();
    assert_eq!(buf[0], 128);

    // Seek from end
    file.seek(SeekFrom::End(-10)).await.unwrap();
    file.read_exact(&mut buf).await.unwrap();
    assert_eq!(buf[0], 246);

    // Seek from current
    file.seek(SeekFrom::Start(100)).await.unwrap();
    file.seek(SeekFrom::Current(50)).await.unwrap();
    file.read_exact(&mut buf).await.unwrap();
    assert_eq!(buf[0], 150);

    cleanup_test_image(path);
}

#[tokio::test]
async fn test_file_truncate() {
    let path = "target/test_truncate.img";
    create_test_image(path, 10).unwrap();

    let file = File::options().read(true).write(true).open(path).unwrap();
    let device = EmbassyBlockDevice::new(file);

    let fs = FileSystem::new(device, FsOptions::new()).await.unwrap();
    let root = fs.root_dir();

    // Create file
    let mut file = root.create_file("trunc.bin").await.unwrap();
    file.write_all(&[0xAB; 1000]).await.unwrap();
    file.flush().await.unwrap();
    let size = file.seek(SeekFrom::End(0)).await.unwrap();
    assert_eq!(size, 1000);

    // Seek to beginning and truncate (truncate at current position)
    file.seek(SeekFrom::Start(0)).await.unwrap();
    file.truncate().await.unwrap();
    file.flush().await.unwrap();
    drop(file);

    // Reopen to verify file size is now 0
    let mut file = root.open_file("trunc.bin").await.unwrap();
    let size = file.seek(SeekFrom::End(0)).await.unwrap();
    assert_eq!(size, 0);

    cleanup_test_image(path);
}

#[tokio::test]
async fn test_directory_iteration() {
    let path = "target/test_iter.img";
    create_test_image(path, 10).unwrap();

    let file = File::options().read(true).write(true).open(path).unwrap();
    let device = EmbassyBlockDevice::new(file);

    let fs = FileSystem::new(device, FsOptions::new()).await.unwrap();
    let root = fs.root_dir();

    // Create several files
    for i in 0..5 {
        let name = format!("file{}.txt", i);
        let mut file = root.create_file(&name).await.unwrap();
        file.write_all(format!("Content {}", i).as_bytes())
            .await
            .unwrap();
        file.flush().await.unwrap();
    }

    // Count entries (skip volume label entries)
    let mut count = 0;
    let mut iter = root.iter();
    while let Some(result) = iter.next().await {
        let entry = result.unwrap();
        if !entry.attributes().contains(FileAttributes::VOLUME_ID) {
            count += 1;
        }
    }

    assert_eq!(count, 5);

    cleanup_test_image(path);
}

#[tokio::test]
async fn test_overwrite_file() {
    let path = "target/test_overwrite.img";
    create_test_image(path, 10).unwrap();

    let file = File::options().read(true).write(true).open(path).unwrap();
    let device = EmbassyBlockDevice::new(file);

    let fs = FileSystem::new(device, FsOptions::new()).await.unwrap();
    let root = fs.root_dir();

    // Create and write
    let mut file = root.create_file("overwrite.txt").await.unwrap();
    file.write_all(b"original content").await.unwrap();
    file.flush().await.unwrap();
    drop(file);

    // Overwrite by truncating first, then writing
    let mut file = root.create_file("overwrite.txt").await.unwrap();
    file.truncate().await.unwrap();
    file.write_all(b"new").await.unwrap();
    file.flush().await.unwrap();
    drop(file);

    // Verify
    let mut file = root.open_file("overwrite.txt").await.unwrap();
    let mut buf = vec![0u8; 20];
    let n = file.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"new");

    cleanup_test_image(path);
}

#[tokio::test]
async fn test_delete_file() {
    let path = "target/test_delete.img";
    create_test_image(path, 10).unwrap();

    let file = File::options().read(true).write(true).open(path).unwrap();
    let device = EmbassyBlockDevice::new(file);

    let fs = FileSystem::new(device, FsOptions::new()).await.unwrap();
    let root = fs.root_dir();

    // Create file
    let mut file = root.create_file("to_delete.txt").await.unwrap();
    file.write_all(b"delete me").await.unwrap();
    file.flush().await.unwrap();
    drop(file);

    // Delete it
    root.remove("to_delete.txt").await.unwrap();

    // Verify it's gone
    let result = root.open_file("to_delete.txt").await;
    assert!(result.is_err());

    cleanup_test_image(path);
}

#[tokio::test]
async fn test_rename_file() {
    let path = "target/test_rename.img";
    create_test_image(path, 10).unwrap();

    let file = File::options().read(true).write(true).open(path).unwrap();
    let device = EmbassyBlockDevice::new(file);

    let fs = FileSystem::new(device, FsOptions::new()).await.unwrap();
    let root = fs.root_dir();

    // Create file
    let mut file = root.create_file("old_name.txt").await.unwrap();
    file.write_all(b"content").await.unwrap();
    file.flush().await.unwrap();
    drop(file);

    // Rename it
    root.rename("old_name.txt", &root, "new_name.txt")
        .await
        .unwrap();

    // Verify old name is gone
    assert!(root.open_file("old_name.txt").await.is_err());

    // Verify new name exists with correct content
    let mut file = root.open_file("new_name.txt").await.unwrap();
    let mut buf = vec![0u8; 10];
    let n = file.read(&mut buf).await.unwrap();
    assert_eq!(&buf[..n], b"content");

    cleanup_test_image(path);
}

/// Test read/write patterns typical in embedded systems
#[tokio::test]
async fn test_embedded_io_patterns() {
    let path = "target/test_embedded_io.img";
    create_test_image(path, 10).unwrap();

    let file = File::options().read(true).write(true).open(path).unwrap();
    let device = EmbassyBlockDevice::new(file);

    let fs = FileSystem::new(device, FsOptions::new()).await.unwrap();
    let root = fs.root_dir();

    // Simulate sensor data logging (small writes)
    let mut log_file = root.create_file("sensor.log").await.unwrap();

    for i in 0..100 {
        let record = format!("T={},H={}\n", 20 + (i % 10), 50 + (i % 20));
        log_file.write_all(record.as_bytes()).await.unwrap();
    }
    log_file.flush().await.unwrap();
    drop(log_file);

    // Verify log file
    let mut log_file = root.open_file("sensor.log").await.unwrap();
    let mut content = Vec::new();
    let mut buf = [0u8; 64];
    loop {
        match log_file.read(&mut buf).await.unwrap() {
            0 => break,
            n => content.extend_from_slice(&buf[..n]),
        }
    }

    let log_str = String::from_utf8(content).unwrap();
    assert!(log_str.contains("T=20,H=50"));
    assert!(log_str.lines().count() == 100);

    cleanup_test_image(path);
}
