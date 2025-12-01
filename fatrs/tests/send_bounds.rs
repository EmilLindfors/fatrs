//! Test that FileSystem and related types are Send when IO is Send

use std::fs::File;
use std::io::{Read as StdRead, Seek as StdSeek, Write as StdWrite};
use std::sync::Arc;

use async_lock::Mutex;
use embedded_io_async::{ErrorType, Read, Seek, SeekFrom, Write};

/// A Send-compatible block device wrapper
struct SendBlockDevice {
    inner: Arc<Mutex<File>>,
}

impl Clone for SendBlockDevice {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl SendBlockDevice {
    fn new(file: File) -> Self {
        Self {
            inner: Arc::new(Mutex::new(file)),
        }
    }
}

impl ErrorType for SendBlockDevice {
    type Error = std::io::Error;
}

impl Read for SendBlockDevice {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        let mut file = self.inner.lock().await;
        file.read(buf)
    }
}

impl Write for SendBlockDevice {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        let mut file = self.inner.lock().await;
        file.write(buf)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        let mut file = self.inner.lock().await;
        file.flush()
    }
}

impl Seek for SendBlockDevice {
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

// Compile-time assertion that T is Send
fn assert_send<T: Send>() {}
fn assert_sync<T: Sync>() {}

#[test]
fn test_send_block_device_is_send() {
    assert_send::<SendBlockDevice>();
}

#[test]
fn test_filesystem_is_send_when_io_is_send() {
    // This test verifies that FileSystem<SendBlockDevice, ...> is Send
    // It will fail to compile if FileSystem is not Send
    assert_send::<
        fatrs::FileSystem<SendBlockDevice, fatrs::DefaultTimeProvider, fatrs::LossyOemCpConverter>,
    >();
}

#[test]
fn test_filesystem_is_sync_when_io_is_send() {
    // async_lock::Mutex<T> is Sync when T: Send
    assert_sync::<
        fatrs::FileSystem<SendBlockDevice, fatrs::DefaultTimeProvider, fatrs::LossyOemCpConverter>,
    >();
}

// Note: tokio::spawn requires Send futures, but embedded-io-async's async fn
// in traits don't return Send futures by design (for embedded compatibility).
// Use tokio::task::spawn_local with LocalSet for concurrent tasks instead.
// See concurrent_access.rs for examples.
