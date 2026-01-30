//! Stack-allocated streaming page buffer.

use crate::{
    adapters::{StackBuffer, AdapterError},
    infrastructure::streaming::{StreamError, SeekFrom},
};
use fatrs_block_device::BlockDevice;

#[cfg(feature = "alloc")]
extern crate alloc;

/// Stack-allocated streaming page buffer with compile-time size.
///
/// This wraps `StackBuffer` and adds async Read/Write/Seek capabilities.
/// Perfect for embedded systems with fixed-size page buffers.
///
/// # Type Parameters
///
/// - `D`: The block device type
/// - `N`: Page size in bytes (must be a multiple of BLOCK_SIZE)
/// - `BLOCK_SIZE`: The block size in bytes (must match the device's block size)
///
/// # Send/Sync Properties
///
/// The Send/Sync properties are automatically inherited from the underlying
/// StackBuffer and device. No manual bounds needed!
///
/// # Examples
///
/// ```ignore
/// use fatrs_adapters::infrastructure::StackPageStream;
///
/// // 4KB pages with 512-byte blocks
/// let device = MyBlockDevice::new();
/// let mut stream = StackPageStream::<_, 4096, 512>::new(device);
///
/// // Use async read/write/seek
/// stream.write(&[1, 2, 3, 4]).await?;
/// stream.seek(SeekFrom::Start(0)).await?;
/// let mut buf = [0u8; 4];
/// stream.read(&mut buf).await?;
/// ```
pub struct StackPageStream<D, const N: usize, const BLOCK_SIZE: usize>
where
    D: BlockDevice<BLOCK_SIZE> + Send + Sync,
    D::Error: core::error::Error + Send + Sync + 'static,
{
    buffer: StackBuffer<D, N, BLOCK_SIZE>,
    position: u64,
}

impl<D, const N: usize, const BLOCK_SIZE: usize> StackPageStream<D, N, BLOCK_SIZE>
where
    D: BlockDevice<BLOCK_SIZE> + Send + Sync,
    D::Error: core::error::Error + Send + Sync + 'static,
{
    /// Create a new streaming page buffer.
    ///
    /// The page size is determined by `N * BLOCK_SIZE`.
    pub fn new(device: D) -> Self {
        Self {
            buffer: StackBuffer::new(device),
            position: 0,
        }
    }

    /// Read data from the stream.
    ///
    /// Reads up to `buf.len()` bytes from the current position.
    /// Returns the number of bytes read.
    ///
    /// Note: This method is internal. Users should use the `embedded_io_async::Read` trait.
    pub(crate) async fn read(&mut self, buf: &mut [u8]) -> Result<usize, StreamError<D::Error>> {
        if buf.is_empty() {
            return Ok(0);
        }

        let page_size = (N * BLOCK_SIZE) as u64;
        let page_num = (self.position / page_size) as u32;
        let page_offset = (self.position % page_size) as usize;

        // Load the page containing current position
        self.buffer
            .load(page_num)
            .await
            .map_err(|e| match e {
                AdapterError::Storage(s) => StreamError::Storage(s),
                _ => StreamError::OutOfBounds,
            })?;

        // Read from current page
        let data = self.buffer.data().map_err(|e| match e {
            AdapterError::Storage(s) => StreamError::Storage(s),
            _ => StreamError::OutOfBounds,
        })?;

        let available = data.len().saturating_sub(page_offset);
        let to_read = buf.len().min(available);

        if to_read > 0 {
            buf[..to_read].copy_from_slice(&data[page_offset..page_offset + to_read]);
            self.position += to_read as u64;
        }

        Ok(to_read)
    }

    /// Write data to the stream.
    ///
    /// Writes `buf.len()` bytes to the current position.
    /// Returns the number of bytes written.
    ///
    /// Note: This method is internal. Users should use the `embedded_io_async::Write` trait.
    pub(crate) async fn write(&mut self, buf: &[u8]) -> Result<usize, StreamError<D::Error>> {
        if buf.is_empty() {
            return Ok(0);
        }

        let page_size = (N * BLOCK_SIZE) as u64;
        let page_num = (self.position / page_size) as u32;
        let page_offset = (self.position % page_size) as usize;

        // Load the page containing current position
        self.buffer
            .load(page_num)
            .await
            .map_err(|e| match e {
                AdapterError::Storage(s) => StreamError::Storage(s),
                _ => StreamError::OutOfBounds,
            })?;

        // Write to current page
        let data = self.buffer.data_mut().map_err(|e| match e {
            AdapterError::Storage(s) => StreamError::Storage(s),
            _ => StreamError::OutOfBounds,
        })?;

        let available = data.len().saturating_sub(page_offset);
        let to_write = buf.len().min(available);

        if to_write > 0 {
            data[page_offset..page_offset + to_write].copy_from_slice(&buf[..to_write]);
            self.position += to_write as u64;
        }

        Ok(to_write)
    }

    /// Flush any uncommitted changes to storage.
    ///
    /// Note: This method is internal. Users should use the `embedded_io_async::Write` trait.
    pub(crate) async fn flush(&mut self) -> Result<(), StreamError<D::Error>> {
        self.buffer.flush().await.map_err(|e| match e {
            AdapterError::Storage(s) => StreamError::Storage(s),
            _ => StreamError::OutOfBounds,
        })
    }

    /// Seek to a new position in the stream.
    ///
    /// Returns the new position from the start of the stream.
    ///
    /// Note: This method is internal. Users should use the `embedded_io_async::Seek` trait.
    pub(crate) async fn seek(&mut self, pos: SeekFrom) -> Result<u64, StreamError<D::Error>> {
        // Flush before seeking to ensure data consistency
        self.flush().await?;

        let size = self.buffer.size().await.map_err(|e| match e {
            AdapterError::Storage(s) => StreamError::Storage(s),
            _ => StreamError::OutOfBounds,
        })?;

        let new_pos = match pos {
            SeekFrom::Start(offset) => offset as i64,
            SeekFrom::Current(offset) => self.position as i64 + offset,
            SeekFrom::End(offset) => size as i64 + offset,
        };

        if new_pos < 0 {
            return Err(StreamError::InvalidSeek);
        }

        let page_size = (N * BLOCK_SIZE) as u64;
        let old_page = self.position / page_size;
        let new_page = new_pos as u64 / page_size;

        // If seeking to a different page, clear the buffer to avoid corruption
        // This ensures the next read/write loads the correct page
        if old_page != new_page {
            self.buffer.clear();
        }

        self.position = new_pos as u64;
        Ok(self.position)
    }

    /// Get the current position in the stream.
    pub fn position(&self) -> u64 {
        self.position
    }

    /// Get the size of the underlying storage in bytes.
    pub async fn size(&mut self) -> Result<u64, StreamError<D::Error>> {
        self.buffer.size().await.map_err(|e| match e {
            AdapterError::Storage(s) => StreamError::Storage(s),
            _ => StreamError::OutOfBounds,
        })
    }
}
