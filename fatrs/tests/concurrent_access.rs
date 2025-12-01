//! Concurrent access integration tests
//!
//! Tests for verifying that the filesystem handles concurrent access correctly,
//! including file locking, multiple readers, and preventing write conflicts.
//!
//! Note: Since embedded-io-async futures are not Send by default, these tests use
//! `tokio::task::spawn_local` with a `LocalSet` for concurrent execution on a
//! single thread. This matches the expected use case in embedded systems where
//! single-threaded executors are common.

use std::fs::File;
use std::io::{Read as StdRead, Seek as StdSeek, Write as StdWrite};
use std::sync::Arc;

use async_lock::{Barrier, Mutex};
use embedded_io_async::{ErrorType, Read, Seek, SeekFrom, Write};
use fatrs::{FileSystem, FsOptions};
use tokio::task::{LocalSet, spawn_local};

/// Thread-safe Embassy-compatible block device wrapper
struct SharedBlockDevice {
    inner: Arc<Mutex<File>>,
}

impl Clone for SharedBlockDevice {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl SharedBlockDevice {
    fn new(file: File) -> Self {
        Self {
            inner: Arc::new(Mutex::new(file)),
        }
    }
}

impl ErrorType for SharedBlockDevice {
    type Error = std::io::Error;
}

impl Read for SharedBlockDevice {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let mut file = self.inner.lock().await;
        file.read(buf)
    }
}

impl Write for SharedBlockDevice {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let mut file = self.inner.lock().await;
        file.write(buf)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        let mut file = self.inner.lock().await;
        file.flush()
    }
}

impl Seek for SharedBlockDevice {
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        let mut file = self.inner.lock().await;
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

    let _ = std::fs::create_dir_all("target");

    let file = File::create(path)?;
    file.set_len((size_mb as u64) * 1024 * 1024)?;
    drop(file);

    let output = Command::new("mkfs.fat")
        .args(["-F", "32", "-n", "TEST", path])
        .output();

    if output.is_err() || !output.as_ref().unwrap().status.success() {
        let file = File::options().read(true).write(true).open(path)?;
        let mut device = SharedBlockDevice::new(file);

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
async fn test_concurrent_reads() {
    let local = LocalSet::new();

    local
        .run_until(async {
            let path = "target/test_concurrent_reads.img";
            create_test_image(path, 10).unwrap();

            let file = File::options().read(true).write(true).open(path).unwrap();
            let device = SharedBlockDevice::new(file);

            let fs = Arc::new(FileSystem::new(device, FsOptions::new()).await.unwrap());

            // Create a test file
            {
                let root = fs.root_dir();
                let mut file = root.create_file("shared.txt").await.unwrap();
                file.write_all(b"This is shared data that multiple readers will access")
                    .await
                    .unwrap();
                file.flush().await.unwrap();
            }

            // Spawn multiple concurrent readers using spawn_local
            let barrier = Arc::new(Barrier::new(4));
            let mut handles = Vec::new();

            for i in 0..4 {
                let fs = fs.clone();
                let barrier = barrier.clone();

                handles.push(spawn_local(async move {
                    // Wait for all tasks to be ready
                    barrier.wait().await;

                    let root = fs.root_dir();
                    let mut file = root.open_file("shared.txt").await.unwrap();

                    let mut buf = vec![0u8; 100];
                    let n = file.read(&mut buf).await.unwrap();

                    // Verify content
                    assert!(n > 0);
                    let content = String::from_utf8_lossy(&buf[..n]);
                    assert!(content.starts_with("This is shared data"));

                    i
                }));
            }

            // Wait for all readers to complete
            let mut results = Vec::new();
            for handle in handles {
                results.push(handle.await.unwrap());
            }

            // All 4 readers should complete successfully
            assert_eq!(results.len(), 4);

            cleanup_test_image(path);
        })
        .await;
}

#[tokio::test]
async fn test_sequential_writes() {
    let local = LocalSet::new();

    local
        .run_until(async {
            let path = "target/test_sequential_writes.img";
            create_test_image(path, 10).unwrap();

            let file = File::options().read(true).write(true).open(path).unwrap();
            let device = SharedBlockDevice::new(file);

            let fs = Arc::new(FileSystem::new(device, FsOptions::new()).await.unwrap());

            // Create different files sequentially from multiple tasks
            let barrier = Arc::new(Barrier::new(4));
            let mut handles = Vec::new();

            for i in 0..4 {
                let fs = fs.clone();
                let barrier = barrier.clone();

                handles.push(spawn_local(async move {
                    // Wait for all tasks to be ready
                    barrier.wait().await;

                    let root = fs.root_dir();
                    let filename = format!("file{}.txt", i);
                    let mut file = root.create_file(&filename).await.unwrap();
                    let content = format!("Content from task {}", i);
                    file.write_all(content.as_bytes()).await.unwrap();
                    file.flush().await.unwrap();

                    i
                }));
            }

            // Wait for all writers
            for handle in handles {
                handle.await.unwrap();
            }

            // Verify all files were created
            let root = fs.root_dir();
            for i in 0..4 {
                let filename = format!("file{}.txt", i);
                let mut file = root.open_file(&filename).await.unwrap();
                let mut buf = vec![0u8; 50];
                let n = file.read(&mut buf).await.unwrap();
                let content = String::from_utf8_lossy(&buf[..n]);
                assert!(content.contains(&format!("Content from task {}", i)));
            }

            cleanup_test_image(path);
        })
        .await;
}

#[tokio::test]
async fn test_concurrent_directory_operations() {
    let local = LocalSet::new();

    local
        .run_until(async {
            let path = "target/test_concurrent_dirs.img";
            create_test_image(path, 10).unwrap();

            let file = File::options().read(true).write(true).open(path).unwrap();
            let device = SharedBlockDevice::new(file);

            let fs = Arc::new(FileSystem::new(device, FsOptions::new()).await.unwrap());

            // Create multiple directories concurrently
            let barrier = Arc::new(Barrier::new(4));
            let mut handles = Vec::new();

            for i in 0..4 {
                let fs = fs.clone();
                let barrier = barrier.clone();

                handles.push(spawn_local(async move {
                    barrier.wait().await;

                    let root = fs.root_dir();
                    let dirname = format!("dir{}", i);
                    root.create_dir(&dirname).await.unwrap();

                    // Create a file in the directory
                    let dir = root.open_dir(&dirname).await.unwrap();
                    let mut file = dir.create_file("nested.txt").await.unwrap();
                    file.write_all(format!("Nested content {}", i).as_bytes())
                        .await
                        .unwrap();
                    file.flush().await.unwrap();

                    i
                }));
            }

            for handle in handles {
                handle.await.unwrap();
            }

            // Verify all directories and files were created
            let root = fs.root_dir();
            for i in 0..4 {
                let dirname = format!("dir{}", i);
                let dir = root.open_dir(&dirname).await.unwrap();
                let mut file = dir.open_file("nested.txt").await.unwrap();
                let mut buf = vec![0u8; 50];
                let n = file.read(&mut buf).await.unwrap();
                let content = String::from_utf8_lossy(&buf[..n]);
                assert!(content.contains(&format!("Nested content {}", i)));
            }

            cleanup_test_image(path);
        })
        .await;
}

#[tokio::test]
async fn test_read_during_write_different_files() {
    let local = LocalSet::new();

    local
        .run_until(async {
            let path = "target/test_read_write_different.img";
            create_test_image(path, 10).unwrap();

            let file = File::options().read(true).write(true).open(path).unwrap();
            let device = SharedBlockDevice::new(file);

            let fs = Arc::new(FileSystem::new(device, FsOptions::new()).await.unwrap());

            // Create a file for reading
            {
                let root = fs.root_dir();
                let mut file = root.create_file("readonly.txt").await.unwrap();
                file.write_all(b"This file will be read").await.unwrap();
                file.flush().await.unwrap();
            }

            let barrier = Arc::new(Barrier::new(2));

            // Spawn a reader task
            let fs_read = fs.clone();
            let barrier_read = barrier.clone();
            let reader = spawn_local(async move {
                barrier_read.wait().await;

                let root = fs_read.root_dir();
                for _ in 0..10 {
                    let mut file = root.open_file("readonly.txt").await.unwrap();
                    let mut buf = vec![0u8; 50];
                    let n = file.read(&mut buf).await.unwrap();
                    assert!(n > 0);
                    tokio::task::yield_now().await;
                }
            });

            // Spawn a writer task (writes to different file)
            let fs_write = fs.clone();
            let barrier_write = barrier.clone();
            let writer = spawn_local(async move {
                barrier_write.wait().await;

                let root = fs_write.root_dir();
                for i in 0..10 {
                    let filename = format!("written{}.txt", i);
                    let mut file = root.create_file(&filename).await.unwrap();
                    file.write_all(format!("Data {}", i).as_bytes())
                        .await
                        .unwrap();
                    file.flush().await.unwrap();
                    tokio::task::yield_now().await;
                }
            });

            // Both should complete without error
            reader.await.unwrap();
            writer.await.unwrap();

            cleanup_test_image(path);
        })
        .await;
}

#[tokio::test]
async fn test_filesystem_stats_concurrent_access() {
    let local = LocalSet::new();

    local
        .run_until(async {
            let path = "target/test_fs_stats_concurrent.img";
            create_test_image(path, 10).unwrap();

            let file = File::options().read(true).write(true).open(path).unwrap();
            let device = SharedBlockDevice::new(file);

            let fs = Arc::new(FileSystem::new(device, FsOptions::new()).await.unwrap());

            let barrier = Arc::new(Barrier::new(3));
            let mut handles = Vec::new();

            // Multiple tasks querying filesystem stats
            for _ in 0..3 {
                let fs = fs.clone();
                let barrier = barrier.clone();

                handles.push(spawn_local(async move {
                    barrier.wait().await;

                    for _ in 0..5 {
                        let stats = fs.stats().await.unwrap();
                        assert!(stats.total_clusters() > 0);
                        tokio::task::yield_now().await;
                    }
                }));
            }

            for handle in handles {
                handle.await.unwrap();
            }

            cleanup_test_image(path);
        })
        .await;
}

#[tokio::test]
async fn test_concurrent_file_creation_unique_names() {
    let local = LocalSet::new();

    local
        .run_until(async {
            let path = "target/test_concurrent_create.img";
            create_test_image(path, 10).unwrap();

            let file = File::options().read(true).write(true).open(path).unwrap();
            let device = SharedBlockDevice::new(file);

            let fs = Arc::new(FileSystem::new(device, FsOptions::new()).await.unwrap());

            let barrier = Arc::new(Barrier::new(8));
            let mut handles = Vec::new();

            // 8 tasks creating files simultaneously
            for i in 0..8 {
                let fs = fs.clone();
                let barrier = barrier.clone();

                handles.push(spawn_local(async move {
                    barrier.wait().await;

                    let root = fs.root_dir();
                    let filename = format!("concurrent{}.bin", i);
                    let mut file = root.create_file(&filename).await.unwrap();

                    // Write unique content
                    let data = vec![i as u8; 1024];
                    file.write_all(&data).await.unwrap();
                    file.flush().await.unwrap();

                    i
                }));
            }

            let results: Vec<_> = futures::future::join_all(handles)
                .await
                .into_iter()
                .map(|r| r.unwrap())
                .collect();

            // All 8 tasks should succeed
            assert_eq!(results.len(), 8);

            // Verify all files exist with correct content
            let root = fs.root_dir();
            for i in 0..8 {
                let filename = format!("concurrent{}.bin", i);
                let mut file = root.open_file(&filename).await.unwrap();
                let mut buf = vec![0u8; 1024];
                file.read_exact(&mut buf).await.unwrap();
                assert!(buf.iter().all(|&b| b == i as u8));
            }

            cleanup_test_image(path);
        })
        .await;
}

#[tokio::test]
async fn test_directory_listing_during_modifications() {
    let local = LocalSet::new();

    local
        .run_until(async {
            let path = "target/test_dir_list_modify.img";
            create_test_image(path, 10).unwrap();

            let file = File::options().read(true).write(true).open(path).unwrap();
            let device = SharedBlockDevice::new(file);

            let fs = Arc::new(FileSystem::new(device, FsOptions::new()).await.unwrap());

            // Pre-create some files
            {
                let root = fs.root_dir();
                for i in 0..5 {
                    let mut file = root.create_file(&format!("pre{}.txt", i)).await.unwrap();
                    file.write_all(b"pre").await.unwrap();
                    file.flush().await.unwrap();
                }
            }

            let barrier = Arc::new(Barrier::new(2));

            // Task that lists directory
            let fs_list = fs.clone();
            let barrier_list = barrier.clone();
            let lister = spawn_local(async move {
                barrier_list.wait().await;

                let root = fs_list.root_dir();
                for _ in 0..5 {
                    let mut iter = root.iter();
                    let mut count = 0;
                    while let Some(result) = iter.next().await {
                        let entry = result.unwrap();
                        if entry.is_file() {
                            count += 1;
                        }
                    }
                    assert!(count >= 5); // At least the pre-created files
                    tokio::task::yield_now().await;
                }
            });

            // Task that creates new files
            let fs_create = fs.clone();
            let barrier_create = barrier.clone();
            let creator = spawn_local(async move {
                barrier_create.wait().await;

                let root = fs_create.root_dir();
                for i in 0..5 {
                    let mut file = root.create_file(&format!("new{}.txt", i)).await.unwrap();
                    file.write_all(b"new").await.unwrap();
                    file.flush().await.unwrap();
                    tokio::task::yield_now().await;
                }
            });

            // Both should complete without error
            lister.await.unwrap();
            creator.await.unwrap();

            cleanup_test_image(path);
        })
        .await;
}
