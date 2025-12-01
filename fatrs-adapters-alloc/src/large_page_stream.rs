//! Large page stream for block devices (heap-allocated)
//!
//! Provides byte-level `Read`/`Write`/`Seek` traits on top of `LargePageBuffer`,
//! enabling efficient I/O with large page sizes (128KB+) for SSDs.

extern crate alloc;

use embedded_io_async::{ErrorKind, Read, Seek, SeekFrom, Write};
use fatrs_adapters_core::BLOCK_SIZE;
use fatrs_block_device::BlockDevice;

use crate::large_page_buffer::{LargePageBuffer, LargePageBufferError};

/// Error type for LargePageStream operations
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum LargePageStreamError<E> {
    /// Underlying page buffer error
    Page(LargePageBufferError<E>),
}

impl<E> From<LargePageBufferError<E>> for LargePageStreamError<E> {
    fn from(e: LargePageBufferError<E>) -> Self {
        LargePageStreamError::Page(e)
    }
}

impl<E: core::fmt::Display> core::fmt::Display for LargePageStreamError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LargePageStreamError::Page(e) => write!(f, "page error: {}", e),
        }
    }
}

impl<E: core::fmt::Debug + core::fmt::Display> core::error::Error for LargePageStreamError<E> {}

impl<E: core::fmt::Debug + core::fmt::Display> embedded_io_async::Error
    for LargePageStreamError<E>
{
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

/// Large page stream providing byte-level access with heap-allocated multi-block buffering
///
/// This adapter wraps a block device and buffers entire large pages in memory,
/// providing the `embedded_io_async` `Read`, `Write`, and `Seek` traits.
///
/// # Use Cases
///
/// - **SSD sequential I/O**: 128KB+ pages reduce command overhead
/// - **Large file transfers**: Amortize syscall cost
/// - **Streaming media**: Buffer ahead for smooth playback
///
/// # Example
///
/// ```ignore
/// use fatrs_adapters_alloc::{LargePageStream, presets};
///
/// // Create 128KB buffered stream for SSD
/// let mut stream = LargePageStream::new(ssd_device, presets::PAGE_128K);
///
/// // Read with 128KB internal buffering
/// let mut buf = vec![0u8; 1024];
/// stream.read(&mut buf).await?;
/// ```
pub struct LargePageStream<D: BlockDevice<BLOCK_SIZE>> {
    /// The underlying page buffer
    buffer: LargePageBuffer<D>,

    /// Current byte offset in the device
    offset: u64,

    /// Cached device size (lazily initialized)
    cached_size: Option<u64>,
}

impl<D: BlockDevice<BLOCK_SIZE>> LargePageStream<D> {
    /// Create a new large page stream with the specified page size
    ///
    /// # Arguments
    /// * `device` - The underlying block device
    /// * `page_size` - Page size in bytes (must be multiple of 512)
    pub fn new(device: D, page_size: usize) -> Self {
        Self {
            buffer: LargePageBuffer::new(device, page_size),
            offset: 0,
            cached_size: None,
        }
    }

    /// Get the device size, caching it for subsequent calls
    async fn device_size(&mut self) -> Result<u64, LargePageBufferError<D::Error>> {
        if let Some(size) = self.cached_size {
            return Ok(size);
        }
        let size = self
            .buffer
            .inner_mut()
            .size()
            .await
            .map_err(LargePageBufferError::Io)?;
        self.cached_size = Some(size);
        Ok(size)
    }

    /// Get the page size in bytes
    pub fn page_size(&self) -> usize {
        self.buffer.page_size()
    }

    /// Returns the inner block device, consuming this stream
    pub fn into_inner(self) -> D {
        self.buffer.into_inner()
    }

    /// Get a reference to the inner block device
    pub fn inner(&self) -> &D {
        self.buffer.inner()
    }

    /// Get a mutable reference to the inner block device
    pub fn inner_mut(&mut self) -> &mut D {
        self.buffer.inner_mut()
    }

    /// Get the current byte offset
    pub fn position(&self) -> u64 {
        self.offset
    }

    /// Check if the buffer has uncommitted writes
    pub fn is_dirty(&self) -> bool {
        self.buffer.is_dirty()
    }

    /// Resize the internal buffer
    ///
    /// This flushes any dirty data and clears the cache.
    pub async fn resize(
        &mut self,
        new_page_size: usize,
    ) -> Result<(), LargePageStreamError<D::Error>> {
        self.buffer.flush().await?;
        self.buffer.resize(new_page_size);
        Ok(())
    }

    /// Get the current page number based on offset
    #[inline]
    fn current_page(&self) -> u32 {
        (self.offset / self.buffer.page_size() as u64) as u32
    }

    /// Get the offset within the current page
    #[inline]
    fn offset_in_page(&self) -> usize {
        (self.offset % self.buffer.page_size() as u64) as usize
    }

    /// Ensure the correct page is loaded in the buffer
    async fn ensure_page_loaded(&mut self) -> Result<(), LargePageStreamError<D::Error>> {
        let page = self.current_page();

        // If we have a dirty page for a different page, flush it first
        if self.buffer.is_dirty() && self.buffer.current_page() != Some(page) {
            self.buffer.flush().await?;
        }

        self.buffer.read_page(page).await?;
        Ok(())
    }

    /// Internal flush implementation
    async fn do_flush(&mut self) -> Result<(), LargePageStreamError<D::Error>> {
        self.buffer.flush().await?;
        Ok(())
    }
}

impl<D: BlockDevice<BLOCK_SIZE>> embedded_io_async::ErrorType for LargePageStream<D>
where
    D::Error: core::fmt::Debug + core::fmt::Display,
{
    type Error = LargePageStreamError<D::Error>;
}

impl<D: BlockDevice<BLOCK_SIZE>> Read for LargePageStream<D>
where
    D::Error: core::fmt::Debug + core::fmt::Display,
{
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }

        // Get device size for EOF detection (cached after first call)
        let device_size = self.device_size().await?;

        // Check if we're already at EOF
        if self.offset >= device_size {
            return Ok(0);
        }

        let mut total_read = 0;
        let mut remaining = buf;
        let page_size = self.buffer.page_size();

        while !remaining.is_empty() && self.offset < device_size {
            self.ensure_page_loaded().await?;

            let page_data = self.buffer.data().expect("page should be loaded");
            let offset_in_page = self.offset_in_page();
            let bytes_available = page_size - offset_in_page;

            // Don't read past EOF
            let bytes_until_eof = (device_size - self.offset) as usize;
            let bytes_to_read = remaining.len().min(bytes_available).min(bytes_until_eof);

            if bytes_to_read == 0 {
                break;
            }

            remaining[..bytes_to_read]
                .copy_from_slice(&page_data[offset_in_page..offset_in_page + bytes_to_read]);

            self.offset += bytes_to_read as u64;
            total_read += bytes_to_read;
            remaining = &mut remaining[bytes_to_read..];
        }

        Ok(total_read)
    }
}

impl<D: BlockDevice<BLOCK_SIZE>> Write for LargePageStream<D>
where
    D::Error: core::fmt::Debug + core::fmt::Display,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut total_written = 0;
        let mut remaining = buf;
        let page_size = self.buffer.page_size();

        while !remaining.is_empty() {
            self.ensure_page_loaded().await?;

            let offset_in_page = self.offset_in_page();
            let bytes_available = page_size - offset_in_page;
            let bytes_to_write = remaining.len().min(bytes_available);

            let page_data = self.buffer.data_mut().expect("page should be loaded");
            page_data[offset_in_page..offset_in_page + bytes_to_write]
                .copy_from_slice(&remaining[..bytes_to_write]);

            self.offset += bytes_to_write as u64;
            total_written += bytes_to_write;
            remaining = &remaining[bytes_to_write..];

            // If we filled this page, flush it before moving to next
            if self.offset_in_page() == 0 && self.buffer.is_dirty() {
                self.buffer.flush().await?;
            }
        }

        Ok(total_written)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        self.do_flush().await
    }
}

impl<D: BlockDevice<BLOCK_SIZE>> Seek for LargePageStream<D>
where
    D::Error: core::fmt::Debug + core::fmt::Display,
{
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        let new_offset = match pos {
            SeekFrom::Start(offset) => offset,
            SeekFrom::End(offset) => {
                let size = self
                    .buffer
                    .inner_mut()
                    .size()
                    .await
                    .map_err(LargePageBufferError::Io)?;
                if offset >= 0 {
                    size.saturating_add(offset as u64)
                } else {
                    size.saturating_sub((-offset) as u64)
                }
            }
            SeekFrom::Current(offset) => {
                if offset >= 0 {
                    self.offset.saturating_add(offset as u64)
                } else {
                    self.offset.saturating_sub((-offset) as u64)
                }
            }
        };

        self.offset = new_offset;
        Ok(self.offset)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::presets;
    use aligned::{A4, Aligned};
    use embedded_io_adapters::tokio_1::FromTokio;
    use embedded_io_async::ErrorType;

    struct TestBlockDevice<
        T: embedded_io_async::Read + embedded_io_async::Write + embedded_io_async::Seek,
    >(T);

    impl<T: embedded_io_async::Read + embedded_io_async::Write + embedded_io_async::Seek> ErrorType
        for TestBlockDevice<T>
    {
        type Error = T::Error;
    }

    impl<T: embedded_io_async::Read + embedded_io_async::Write + embedded_io_async::Seek>
        BlockDevice<512> for TestBlockDevice<T>
    {
        type Error = T::Error;
        type Align = A4;

        async fn read(
            &mut self,
            block_address: u32,
            data: &mut [Aligned<Self::Align, [u8; 512]>],
        ) -> Result<(), Self::Error> {
            self.0
                .seek(SeekFrom::Start((block_address as u64) * 512))
                .await?;
            for block in data {
                self.0.read(&mut block[..]).await?;
            }
            Ok(())
        }

        async fn write(
            &mut self,
            block_address: u32,
            data: &[Aligned<Self::Align, [u8; 512]>],
        ) -> Result<(), Self::Error> {
            self.0
                .seek(SeekFrom::Start((block_address as u64) * 512))
                .await?;
            for block in data {
                self.0.write(&block[..]).await?;
            }
            Ok(())
        }

        async fn size(&mut self) -> Result<u64, Self::Error> {
            Ok(1024 * 1024) // 1MB test device
        }
    }

    #[tokio::test]
    async fn test_large_page_stream_read() {
        let mut data = vec![0u8; 1024 * 1024];
        data[0..4].copy_from_slice(b"TEST");
        data[presets::PAGE_64K..presets::PAGE_64K + 4].copy_from_slice(b"PAGE");

        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut stream = LargePageStream::new(block_dev, presets::PAGE_64K);

        // Read first 4 bytes
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"TEST");

        // Seek to second page and read
        stream
            .seek(SeekFrom::Start(presets::PAGE_64K as u64))
            .await
            .unwrap();
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"PAGE");
    }

    #[tokio::test]
    async fn test_large_page_stream_write() {
        let data = vec![0u8; 1024 * 1024];
        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut stream = LargePageStream::new(block_dev, presets::PAGE_64K);

        // Write across page boundary
        stream
            .seek(SeekFrom::Start(presets::PAGE_64K as u64 - 2))
            .await
            .unwrap();
        stream.write_all(b"SPAN").await.unwrap();
        stream.flush().await.unwrap();

        // Verify
        let inner = stream.into_inner().0.into_inner().into_inner();
        assert_eq!(
            &inner[presets::PAGE_64K - 2..presets::PAGE_64K + 2],
            b"SPAN"
        );
    }

    #[tokio::test]
    async fn test_resize() {
        let data = vec![0u8; 1024 * 1024];
        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut stream = LargePageStream::new(block_dev, presets::PAGE_32K);

        assert_eq!(stream.page_size(), 32 * 1024);

        // Resize to 128KB
        stream.resize(presets::PAGE_128K).await.unwrap();
        assert_eq!(stream.page_size(), 128 * 1024);
    }
}
