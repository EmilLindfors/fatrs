//! Page-based stream adapter for block devices
//!
//! Provides byte-level `Read`/`Write`/`Seek` traits on top of a `PageBuffer`,
//! enabling efficient I/O with larger page sizes (e.g., 4KB) instead of
//! single 512-byte blocks.
//!
//! # Example
//!
//! ```ignore
//! use fatrs_adapters_core::PageStream;
//! use embedded_io_async::{Read, Write, Seek};
//!
//! // Create a 4KB page stream over an SD card
//! let mut stream: PageStream<_, 8> = PageStream::new(sd_card);
//!
//! // Read 100 bytes starting at offset 0
//! let mut buf = [0u8; 100];
//! stream.read(&mut buf).await?;
//!
//! // Seek and write
//! stream.seek(SeekFrom::Start(4096)).await?;
//! stream.write_all(b"Hello").await?;
//! stream.flush().await?;
//! ```

use embedded_io_async::{ErrorKind, Read, Seek, SeekFrom, Write};
use fatrs_block_device::BlockDevice;

use crate::page_buffer::{BLOCK_SIZE, PageBuffer, PageBufferError};

/// Error type for PageStream operations
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum PageStreamError<E> {
    /// Underlying page buffer error
    Page(PageBufferError<E>),
}

impl<E> From<PageBufferError<E>> for PageStreamError<E> {
    fn from(e: PageBufferError<E>) -> Self {
        PageStreamError::Page(e)
    }
}

impl<E: core::fmt::Display> core::fmt::Display for PageStreamError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            PageStreamError::Page(e) => write!(f, "page error: {}", e),
        }
    }
}

impl<E: core::fmt::Debug + core::fmt::Display> core::error::Error for PageStreamError<E> {}

impl<E: core::fmt::Debug + core::fmt::Display> embedded_io_async::Error for PageStreamError<E> {
    fn kind(&self) -> ErrorKind {
        ErrorKind::Other
    }
}

/// Page-based stream providing byte-level access with multi-block buffering
///
/// This adapter wraps a block device and buffers entire pages (multiple blocks)
/// in memory, providing the `embedded_io_async` `Read`, `Write`, and `Seek` traits.
///
/// Compared to `BufStream` which buffers a single 512-byte block, `PageStream`
/// buffers `BLOCKS_PER_PAGE * 512` bytes, which can significantly improve
/// performance for sequential I/O patterns.
///
/// # Type Parameters
///
/// - `D`: The underlying block device type (must be `BlockDevice<512>`)
/// - `BLOCKS_PER_PAGE`: Number of 512-byte blocks per page (e.g., 8 for 4KB pages)
///
/// # Performance
///
/// `PageStream` is most beneficial when:
/// - Reading/writing sequentially within pages (avoids repeated I/O)
/// - Working with file formats that naturally align to page boundaries
/// - The underlying storage benefits from larger transfer sizes
pub struct PageStream<D: BlockDevice<BLOCK_SIZE>, const BLOCKS_PER_PAGE: usize> {
    /// The underlying page buffer
    buffer: PageBuffer<D, BLOCKS_PER_PAGE>,

    /// Current byte offset in the device
    offset: u64,

    /// Cached device size (lazily initialized)
    cached_size: Option<u64>,
}

impl<D: BlockDevice<BLOCK_SIZE>, const BLOCKS_PER_PAGE: usize> PageStream<D, BLOCKS_PER_PAGE> {
    /// The page size in bytes
    pub const PAGE_SIZE: usize = BLOCKS_PER_PAGE * BLOCK_SIZE;

    /// Create a new page stream wrapping a block device
    pub fn new(device: D) -> Self {
        Self {
            buffer: PageBuffer::new(device),
            offset: 0,
            cached_size: None,
        }
    }

    /// Get the device size, caching it for subsequent calls
    async fn device_size(&mut self) -> Result<u64, PageBufferError<D::Error>> {
        if let Some(size) = self.cached_size {
            return Ok(size);
        }
        let size = self
            .buffer
            .inner_mut()
            .size()
            .await
            .map_err(PageBufferError::from)?;
        self.cached_size = Some(size);
        Ok(size)
    }

    /// Returns the inner block device, consuming this stream
    ///
    /// # Warning
    /// If the buffer is dirty, changes will be lost. Call `flush()` first.
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

    /// Get the current page number based on offset
    #[inline]
    fn current_page(&self) -> u32 {
        (self.offset / Self::PAGE_SIZE as u64) as u32
    }

    /// Get the offset within the current page
    #[inline]
    fn offset_in_page(&self) -> usize {
        (self.offset % Self::PAGE_SIZE as u64) as usize
    }

    /// Ensure the correct page is loaded in the buffer
    async fn ensure_page_loaded(&mut self) -> Result<(), PageStreamError<D::Error>> {
        let page = self.current_page();

        // If we have a dirty page for a different page, flush it first
        if self.buffer.is_dirty() && self.buffer.current_page() != Some(page) {
            self.buffer.flush().await?;
        }

        self.buffer.read_page(page).await?;
        Ok(())
    }

    /// Internal flush implementation
    async fn do_flush(&mut self) -> Result<(), PageStreamError<D::Error>> {
        self.buffer.flush().await?;
        Ok(())
    }
}

impl<D: BlockDevice<BLOCK_SIZE>, const BLOCKS_PER_PAGE: usize> embedded_io_async::ErrorType
    for PageStream<D, BLOCKS_PER_PAGE>
where
    D::Error: core::fmt::Debug + core::fmt::Display,
{
    type Error = PageStreamError<D::Error>;
}

impl<D: BlockDevice<BLOCK_SIZE>, const BLOCKS_PER_PAGE: usize> Read
    for PageStream<D, BLOCKS_PER_PAGE>
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

        while !remaining.is_empty() && self.offset < device_size {
            // Ensure current page is loaded
            self.ensure_page_loaded().await?;

            let page_data = self.buffer.data().expect("page should be loaded");
            let offset_in_page = self.offset_in_page();
            let bytes_available = Self::PAGE_SIZE - offset_in_page;

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

impl<D: BlockDevice<BLOCK_SIZE>, const BLOCKS_PER_PAGE: usize> Write
    for PageStream<D, BLOCKS_PER_PAGE>
where
    D::Error: core::fmt::Debug + core::fmt::Display,
{
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        if buf.is_empty() {
            return Ok(0);
        }

        let mut total_written = 0;
        let mut remaining = buf;

        while !remaining.is_empty() {
            // Ensure current page is loaded (for RMW if writing partial page)
            self.ensure_page_loaded().await?;

            let offset_in_page = self.offset_in_page();
            let bytes_available = Self::PAGE_SIZE - offset_in_page;
            let bytes_to_write = remaining.len().min(bytes_available);

            // Get mutable access to page data
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

impl<D: BlockDevice<BLOCK_SIZE>, const BLOCKS_PER_PAGE: usize> Seek
    for PageStream<D, BLOCKS_PER_PAGE>
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
                    .map_err(PageBufferError::from)?;
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

/// Type alias for 4KB page stream (8 blocks per page)
pub type PageStream4K<D> = PageStream<D, 8>;

/// Type alias for 2KB page stream (4 blocks per page)
pub type PageStream2K<D> = PageStream<D, 4>;

/// Type alias for 8KB page stream (16 blocks per page)
pub type PageStream8K<D> = PageStream<D, 16>;

#[cfg(test)]
mod tests {
    use super::*;
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
            Ok(64 * 1024) // 64KB test device
        }
    }

    #[tokio::test]
    async fn test_read_within_page() {
        let mut data = vec![0u8; 64 * 1024];
        data[0..4].copy_from_slice(b"TEST");
        data[100..104].copy_from_slice(b"DATA");

        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut stream: PageStream<_, 8> = PageStream::new(block_dev);

        // Read first 4 bytes
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"TEST");
        assert_eq!(stream.position(), 4);

        // Seek and read more
        stream.seek(SeekFrom::Start(100)).await.unwrap();
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"DATA");
    }

    #[tokio::test]
    async fn test_read_across_pages() {
        let mut data = vec![0u8; 64 * 1024];
        // Write data that spans page boundary (page 0 ends at 4096)
        data[4094..4098].copy_from_slice(b"SPAN");

        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut stream: PageStream<_, 8> = PageStream::new(block_dev);

        stream.seek(SeekFrom::Start(4094)).await.unwrap();
        let mut buf = [0u8; 4];
        stream.read_exact(&mut buf).await.unwrap();
        assert_eq!(&buf, b"SPAN");
        assert_eq!(stream.position(), 4098);
    }

    #[tokio::test]
    async fn test_write_within_page() {
        let data = vec![0u8; 64 * 1024];
        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut stream: PageStream<_, 8> = PageStream::new(block_dev);

        // Write some data
        stream.write_all(b"Hello, World!").await.unwrap();
        stream.flush().await.unwrap();

        // Verify by reading back
        let inner = stream.into_inner().0.into_inner().into_inner();
        assert_eq!(&inner[0..13], b"Hello, World!");
    }

    #[tokio::test]
    async fn test_write_across_pages() {
        let data = vec![0u8; 64 * 1024];
        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut stream: PageStream<_, 8> = PageStream::new(block_dev);

        // Seek near end of first page
        stream.seek(SeekFrom::Start(4090)).await.unwrap();

        // Write data that spans page boundary
        let write_data = b"This spans the page boundary!";
        stream.write_all(write_data).await.unwrap();
        stream.flush().await.unwrap();

        // Verify
        let inner = stream.into_inner().0.into_inner().into_inner();
        assert_eq!(&inner[4090..4090 + write_data.len()], write_data);
    }

    #[tokio::test]
    async fn test_seek_modes() {
        let data = vec![0u8; 64 * 1024];
        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut stream: PageStream<_, 8> = PageStream::new(block_dev);

        // SeekFrom::Start
        assert_eq!(stream.seek(SeekFrom::Start(100)).await.unwrap(), 100);
        assert_eq!(stream.position(), 100);

        // SeekFrom::Current positive
        assert_eq!(stream.seek(SeekFrom::Current(50)).await.unwrap(), 150);
        assert_eq!(stream.position(), 150);

        // SeekFrom::Current negative
        assert_eq!(stream.seek(SeekFrom::Current(-25)).await.unwrap(), 125);
        assert_eq!(stream.position(), 125);

        // SeekFrom::End
        assert_eq!(
            stream.seek(SeekFrom::End(-100)).await.unwrap(),
            64 * 1024 - 100
        );
    }

    #[tokio::test]
    async fn test_large_sequential_write() {
        let data = vec![0u8; 64 * 1024];
        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut stream: PageStream<_, 8> = PageStream::new(block_dev);

        // Write multiple pages worth of data
        let write_data = vec![b'X'; 12288]; // 3 pages
        stream.write_all(&write_data).await.unwrap();
        stream.flush().await.unwrap();

        // Verify
        let inner = stream.into_inner().0.into_inner().into_inner();
        assert!(inner[..12288].iter().all(|&b| b == b'X'));
        assert_eq!(inner[12288], 0); // Next byte should be zero
    }

    #[tokio::test]
    async fn test_type_aliases() {
        let data = vec![0u8; 64 * 1024];
        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));

        let stream: PageStream4K<_> = PageStream4K::new(block_dev);
        assert_eq!(
            PageStream4K::<TestBlockDevice<FromTokio<std::io::Cursor<Vec<u8>>>>>::PAGE_SIZE,
            4096
        );
        drop(stream);
    }
}
