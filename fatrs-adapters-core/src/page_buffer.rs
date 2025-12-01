//! Page buffering for block devices
//!
//! Handles conversion between 512-byte blocks (SD card native) and larger pages
//! (e.g., 4KB). This is useful when higher-level code works in page-sized units
//! but the underlying storage uses 512-byte blocks.
//!
//! # Example
//!
//! ```ignore
//! use fatrs_adapters_core::PageBuffer;
//!
//! // Wrap a 512-byte block device to present a 4KB page interface (8 blocks per page)
//! let mut page_device: PageBuffer<_, 8> = PageBuffer::new(sd_card);
//!
//! // Read page 0
//! page_device.read_page(0).await?;
//! let data = page_device.data().unwrap();
//! ```

use aligned::Aligned;
use fatrs_block_device::BlockDevice;

/// Standard block size for SD cards (512 bytes)
pub const BLOCK_SIZE: usize = 512;

crate::define_adapter_error! {
    /// Error type for PageBuffer operations
    #[derive(Copy, Clone, PartialEq, Eq)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    pub enum PageBufferError<E> {
        /// Underlying block device error
        Io(E) => "IO error: {}",
        /// Buffer contains uncommitted writes for a different page
        DirtyPageConflict { current_page: u32, requested_page: u32 } => "dirty page conflict: buffer has page {} but page {} requested",
        /// No page is currently loaded
        NoPageLoaded => "no page loaded",
    }
}

/// Page buffer that aggregates multiple 512-byte blocks into larger pages
///
/// This buffer provides:
/// - Conversion between page numbers and block addresses
/// - Dirty tracking for write-back optimization
/// - DMA-safe alignment (inherits from underlying device)
///
/// # Type Parameters
///
/// - `D`: The underlying block device type (must be `BlockDevice<512>`)
/// - `BLOCKS_PER_PAGE`: Number of 512-byte blocks per page
///
/// The page size is `BLOCKS_PER_PAGE * 512` bytes.
///
/// # Common Configurations
///
/// | BLOCKS_PER_PAGE | Page Size | Use Case |
/// |-----------------|-----------|----------|
/// | 8 | 4KB | Standard 4KB pages (most common) |
/// | 4 | 2KB | 2KB pages |
/// | 16 | 8KB | 8KB pages for larger transfers |
/// | 1 | 512B | Single block (minimal buffering) |
pub struct PageBuffer<D: BlockDevice<BLOCK_SIZE>, const BLOCKS_PER_PAGE: usize> {
    /// The underlying block device
    inner: D,

    /// Aligned buffer for page data - stored as array of blocks
    buffer: [Aligned<D::Align, [u8; BLOCK_SIZE]>; BLOCKS_PER_PAGE],

    /// Whether the buffer contains uncommitted writes
    dirty: bool,

    /// The current page number loaded in the buffer (if any)
    current_page: Option<u32>,
}

impl<D: BlockDevice<BLOCK_SIZE>, const BLOCKS_PER_PAGE: usize> PageBuffer<D, BLOCKS_PER_PAGE> {
    /// The page size in bytes (BLOCKS_PER_PAGE * 512)
    pub const PAGE_SIZE: usize = BLOCKS_PER_PAGE * BLOCK_SIZE;

    /// Create a new page buffer wrapping a block device
    pub fn new(inner: D) -> Self {
        Self {
            inner,
            buffer: core::array::from_fn(|_| Aligned([0u8; BLOCK_SIZE])),
            dirty: false,
            current_page: None,
        }
    }

    /// Returns the inner block device, consuming this buffer
    ///
    /// # Warning
    /// If the buffer is dirty, changes will be lost. Call `flush()` first.
    pub fn into_inner(self) -> D {
        self.inner
    }

    /// Get a reference to the inner block device
    pub fn inner(&self) -> &D {
        &self.inner
    }

    /// Get a mutable reference to the inner block device
    ///
    /// # Warning
    /// Direct writes to the inner device may invalidate the buffer state.
    pub fn inner_mut(&mut self) -> &mut D {
        &mut self.inner
    }

    /// Get the current page number (if loaded)
    pub fn current_page(&self) -> Option<u32> {
        self.current_page
    }

    /// Check if buffer is dirty (has uncommitted writes)
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Convert page number to starting block address
    #[inline]
    pub const fn page_to_block_address(page_num: u32) -> u32 {
        page_num * BLOCKS_PER_PAGE as u32
    }

    /// Convert block address to page number
    #[inline]
    pub const fn block_to_page_address(block_num: u32) -> u32 {
        block_num / BLOCKS_PER_PAGE as u32
    }

    /// Read a page from the block device into the buffer
    ///
    /// Reads `BLOCKS_PER_PAGE` consecutive blocks starting at the page's block address.
    ///
    /// # Errors
    /// Returns `DirtyPageConflict` if the buffer contains uncommitted writes for a different page.
    pub async fn read_page(&mut self, page_num: u32) -> Result<(), PageBufferError<D::Error>> {
        // Check if we already have this page
        if self.current_page == Some(page_num) {
            return Ok(());
        }

        // Fail if we have a dirty page that's different
        if self.dirty {
            if let Some(current) = self.current_page {
                return Err(PageBufferError::DirtyPageConflict {
                    current_page: current,
                    requested_page: page_num,
                });
            }
        }

        let block_address = Self::page_to_block_address(page_num);

        // Read all blocks into our buffer
        self.inner.read(block_address, &mut self.buffer).await?;

        self.current_page = Some(page_num);
        self.dirty = false;

        Ok(())
    }

    /// Write the buffer contents to a page on the block device
    ///
    /// Writes `BLOCKS_PER_PAGE` consecutive blocks starting at the page's block address.
    pub async fn write_page(&mut self, page_num: u32) -> Result<(), PageBufferError<D::Error>> {
        let block_address = Self::page_to_block_address(page_num);

        // Write all blocks from our buffer
        self.inner.write(block_address, &self.buffer).await?;

        self.current_page = Some(page_num);
        self.dirty = false;

        Ok(())
    }

    /// Get the buffered page data as a byte slice
    ///
    /// Returns `None` if no page is currently loaded.
    pub fn data(&self) -> Option<&[u8]> {
        if self.current_page.is_some() {
            // Safe: buffer is contiguous array of aligned blocks
            Some(unsafe {
                core::slice::from_raw_parts(self.buffer.as_ptr() as *const u8, Self::PAGE_SIZE)
            })
        } else {
            None
        }
    }

    /// Get the buffered page data as a mutable byte slice and mark as dirty
    ///
    /// Returns `None` if no page is currently loaded.
    pub fn data_mut(&mut self) -> Option<&mut [u8]> {
        if self.current_page.is_some() {
            self.dirty = true;
            // Safe: buffer is contiguous array of aligned blocks
            Some(unsafe {
                core::slice::from_raw_parts_mut(
                    self.buffer.as_mut_ptr() as *mut u8,
                    Self::PAGE_SIZE,
                )
            })
        } else {
            None
        }
    }

    /// Copy data from the buffer to a destination slice
    ///
    /// Returns the number of bytes copied (min of dest.len() and PAGE_SIZE).
    ///
    /// # Errors
    /// Returns an error if no page is loaded.
    pub fn copy_to(&self, dest: &mut [u8]) -> Result<usize, PageBufferError<D::Error>> {
        let data = self.data().ok_or(PageBufferError::NoPageLoaded)?;
        let len = dest.len().min(Self::PAGE_SIZE);
        dest[..len].copy_from_slice(&data[..len]);
        Ok(len)
    }

    /// Copy data into the buffer and mark as dirty
    ///
    /// Sets the current page to `page_num`. If `src` is smaller than PAGE_SIZE,
    /// the remaining bytes are zeroed.
    pub fn copy_from(&mut self, src: &[u8], page_num: u32) {
        let len = src.len().min(Self::PAGE_SIZE);

        // Safe: buffer is contiguous array of aligned blocks
        let data = unsafe {
            core::slice::from_raw_parts_mut(self.buffer.as_mut_ptr() as *mut u8, Self::PAGE_SIZE)
        };

        data[..len].copy_from_slice(&src[..len]);

        // Zero the rest if src is smaller
        if len < Self::PAGE_SIZE {
            data[len..].fill(0);
        }

        self.dirty = true;
        self.current_page = Some(page_num);
    }

    /// Flush the buffer if dirty
    ///
    /// If the buffer contains uncommitted writes, write them to the device.
    pub async fn flush(&mut self) -> Result<(), PageBufferError<D::Error>> {
        if self.dirty {
            if let Some(page_num) = self.current_page {
                self.write_page(page_num).await?;
            }
        }
        Ok(())
    }

    /// Clear the buffer state without writing
    ///
    /// # Warning
    /// If the buffer is dirty, changes will be lost.
    pub fn clear(&mut self) {
        self.dirty = false;
        self.current_page = None;
    }

    /// Get the size of the underlying device in pages
    pub async fn size_in_pages(&mut self) -> Result<u64, PageBufferError<D::Error>> {
        let bytes = self.inner.size().await?;
        Ok(bytes / Self::PAGE_SIZE as u64)
    }

    /// Read multiple contiguous pages directly, bypassing the internal buffer
    ///
    /// This is more efficient for bulk reads as it avoids copying through
    /// the internal buffer. The destination buffer must be at least
    /// `num_pages * PAGE_SIZE` bytes.
    ///
    /// # Arguments
    /// * `start_page` - The first page number to read
    /// * `dest` - Destination buffer (must be at least `num_pages * PAGE_SIZE` bytes)
    /// * `num_pages` - Number of pages to read
    ///
    /// # Returns
    /// The number of bytes read.
    pub async fn read_pages_direct(
        &mut self,
        start_page: u32,
        dest: &mut [u8],
        num_pages: usize,
    ) -> Result<usize, PageBufferError<D::Error>> {
        let total_bytes = num_pages * Self::PAGE_SIZE;
        assert!(dest.len() >= total_bytes, "destination buffer too small");

        for i in 0..num_pages {
            let page_num = start_page + i as u32;
            let block_addr = Self::page_to_block_address(page_num);
            let offset = i * Self::PAGE_SIZE;

            // Create a temporary aligned buffer for this page
            let mut temp_buffer: [Aligned<D::Align, [u8; BLOCK_SIZE]>; BLOCKS_PER_PAGE] =
                core::array::from_fn(|_| Aligned([0u8; BLOCK_SIZE]));

            self.inner.read(block_addr, &mut temp_buffer).await?;

            // Copy to destination
            let src = unsafe {
                core::slice::from_raw_parts(temp_buffer.as_ptr() as *const u8, Self::PAGE_SIZE)
            };
            dest[offset..offset + Self::PAGE_SIZE].copy_from_slice(src);
        }

        Ok(total_bytes)
    }

    /// Write multiple contiguous pages directly, bypassing the internal buffer
    ///
    /// This is more efficient for bulk writes as it avoids copying through
    /// the internal buffer.
    ///
    /// # Arguments
    /// * `start_page` - The first page number to write
    /// * `src` - Source buffer (must be at least `num_pages * PAGE_SIZE` bytes)
    /// * `num_pages` - Number of pages to write
    ///
    /// # Returns
    /// The number of bytes written.
    pub async fn write_pages_direct(
        &mut self,
        start_page: u32,
        src: &[u8],
        num_pages: usize,
    ) -> Result<usize, PageBufferError<D::Error>> {
        let total_bytes = num_pages * Self::PAGE_SIZE;
        assert!(src.len() >= total_bytes, "source buffer too small");

        for i in 0..num_pages {
            let page_num = start_page + i as u32;
            let block_addr = Self::page_to_block_address(page_num);
            let offset = i * Self::PAGE_SIZE;

            // Create a temporary aligned buffer for this page
            let mut temp_buffer: [Aligned<D::Align, [u8; BLOCK_SIZE]>; BLOCKS_PER_PAGE] =
                core::array::from_fn(|_| Aligned([0u8; BLOCK_SIZE]));

            // Copy from source to temp buffer
            let dest = unsafe {
                core::slice::from_raw_parts_mut(
                    temp_buffer.as_mut_ptr() as *mut u8,
                    Self::PAGE_SIZE,
                )
            };
            dest.copy_from_slice(&src[offset..offset + Self::PAGE_SIZE]);

            self.inner.write(block_addr, &temp_buffer).await?;
        }

        // Invalidate our internal buffer since we wrote directly
        self.current_page = None;
        self.dirty = false;

        Ok(total_bytes)
    }

    /// Read and modify a page atomically
    ///
    /// Reads the page into the buffer (if not already loaded), calls the modifier
    /// function, and marks the buffer as dirty if modified.
    ///
    /// # Example
    /// ```ignore
    /// page_buf.modify_page(0, |data| {
    ///     data[0..4].copy_from_slice(b"test");
    ///     true // Return true to mark as modified
    /// }).await?;
    /// ```
    pub async fn modify_page<F>(
        &mut self,
        page_num: u32,
        modifier: F,
    ) -> Result<(), PageBufferError<D::Error>>
    where
        F: FnOnce(&mut [u8]) -> bool,
    {
        self.read_page(page_num).await?;

        // Safe: buffer is contiguous array of aligned blocks
        let data = unsafe {
            core::slice::from_raw_parts_mut(self.buffer.as_mut_ptr() as *mut u8, Self::PAGE_SIZE)
        };

        if modifier(data) {
            self.dirty = true;
        }
        Ok(())
    }
}

impl<D: BlockDevice<BLOCK_SIZE> + Default, const BLOCKS_PER_PAGE: usize> Default
    for PageBuffer<D, BLOCKS_PER_PAGE>
{
    fn default() -> Self {
        Self::new(D::default())
    }
}

/// Type alias for 4KB page buffer (8 blocks per page) - most common
pub type PageBuffer4K<D> = PageBuffer<D, 8>;

/// Type alias for 2KB page buffer (4 blocks per page)
pub type PageBuffer2K<D> = PageBuffer<D, 4>;

/// Type alias for 8KB page buffer (16 blocks per page)
pub type PageBuffer8K<D> = PageBuffer<D, 16>;

/// Type alias for 1KB page buffer (2 blocks per page)
pub type PageBuffer1K<D> = PageBuffer<D, 2>;

#[cfg(test)]
mod tests {
    use super::*;
    use aligned::A4;
    use embedded_io_adapters::tokio_1::FromTokio;
    use embedded_io_async::{ErrorType, Read, Seek, SeekFrom, Write};

    struct TestBlockDevice<T: Read + Write + Seek>(T);

    impl<T: Read + Write + Seek> ErrorType for TestBlockDevice<T> {
        type Error = T::Error;
    }

    impl<T: Read + Write + Seek> BlockDevice<512> for TestBlockDevice<T> {
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

    // Helper type for tests
    type TestDevice = TestBlockDevice<FromTokio<std::io::Cursor<Vec<u8>>>>;

    #[test]
    fn test_page_to_block_conversion() {
        // Use const functions directly - no need to instantiate
        assert_eq!(PageBuffer::<TestDevice, 8>::page_to_block_address(0), 0);
        assert_eq!(PageBuffer::<TestDevice, 8>::page_to_block_address(1), 8);
        assert_eq!(PageBuffer::<TestDevice, 8>::page_to_block_address(10), 80);
    }

    #[test]
    fn test_block_to_page_conversion() {
        assert_eq!(PageBuffer::<TestDevice, 8>::block_to_page_address(0), 0);
        assert_eq!(PageBuffer::<TestDevice, 8>::block_to_page_address(7), 0);
        assert_eq!(PageBuffer::<TestDevice, 8>::block_to_page_address(8), 1);
        assert_eq!(PageBuffer::<TestDevice, 8>::block_to_page_address(15), 1);
        assert_eq!(PageBuffer::<TestDevice, 8>::block_to_page_address(80), 10);
    }

    #[test]
    fn test_page_size_constant() {
        assert_eq!(PageBuffer::<TestDevice, 8>::PAGE_SIZE, 4096);
        assert_eq!(PageBuffer::<TestDevice, 4>::PAGE_SIZE, 2048);
        assert_eq!(PageBuffer::<TestDevice, 16>::PAGE_SIZE, 8192);
    }

    #[tokio::test]
    async fn test_read_page() {
        let mut data = vec![0u8; 64 * 1024];
        // Fill page 0 with 'A's
        data[..4096].fill(b'A');
        // Fill page 1 with 'B's
        data[4096..8192].fill(b'B');

        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut page_buf: PageBuffer<_, 8> = PageBuffer::new(block_dev);

        // Read page 0
        page_buf.read_page(0).await.unwrap();
        assert_eq!(page_buf.current_page(), Some(0));
        assert!(!page_buf.is_dirty());

        let page_data = page_buf.data().unwrap();
        assert_eq!(page_data.len(), 4096);
        assert!(page_data.iter().all(|&b| b == b'A'));

        // Read page 1
        page_buf.read_page(1).await.unwrap();
        assert_eq!(page_buf.current_page(), Some(1));

        let page_data = page_buf.data().unwrap();
        assert!(page_data.iter().all(|&b| b == b'B'));
    }

    #[tokio::test]
    async fn test_write_page() {
        let data = vec![0u8; 64 * 1024];
        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut page_buf: PageBuffer<_, 8> = PageBuffer::new(block_dev);

        // Copy data into buffer
        let write_data = vec![b'X'; 4096];
        page_buf.copy_from(&write_data, 2);

        assert!(page_buf.is_dirty());
        assert_eq!(page_buf.current_page(), Some(2));

        // Flush
        page_buf.flush().await.unwrap();
        assert!(!page_buf.is_dirty());

        // Verify by reading back
        let inner = page_buf.into_inner().0.into_inner().into_inner();
        let page2_start = 2 * 4096;
        assert!(
            inner[page2_start..page2_start + 4096]
                .iter()
                .all(|&b| b == b'X')
        );
    }

    #[tokio::test]
    async fn test_dirty_page_conflict() {
        let data = vec![0u8; 64 * 1024];
        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut page_buf: PageBuffer<_, 8> = PageBuffer::new(block_dev);

        // Load page 0 and modify it
        page_buf.read_page(0).await.unwrap();
        let _ = page_buf.data_mut().unwrap(); // Marks as dirty

        // Try to read a different page without flushing
        let result = page_buf.read_page(1).await;
        assert!(matches!(
            result,
            Err(PageBufferError::DirtyPageConflict {
                current_page: 0,
                requested_page: 1
            })
        ));
    }

    #[tokio::test]
    async fn test_modify_page() {
        let data = vec![0u8; 64 * 1024];
        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut page_buf: PageBuffer<_, 8> = PageBuffer::new(block_dev);

        // Modify page 0
        page_buf
            .modify_page(0, |data| {
                data[0..4].copy_from_slice(b"TEST");
                true
            })
            .await
            .unwrap();

        assert!(page_buf.is_dirty());

        // Flush and verify
        page_buf.flush().await.unwrap();

        let inner = page_buf.into_inner().0.into_inner().into_inner();
        assert_eq!(&inner[0..4], b"TEST");
    }

    #[tokio::test]
    async fn test_read_pages_direct() {
        let mut data = vec![0u8; 64 * 1024];
        data[..4096].fill(b'A');
        data[4096..8192].fill(b'B');
        data[8192..12288].fill(b'C');

        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut page_buf: PageBuffer<_, 8> = PageBuffer::new(block_dev);

        let mut dest = vec![0u8; 12288];
        let bytes_read = page_buf.read_pages_direct(0, &mut dest, 3).await.unwrap();

        assert_eq!(bytes_read, 12288);
        assert!(dest[..4096].iter().all(|&b| b == b'A'));
        assert!(dest[4096..8192].iter().all(|&b| b == b'B'));
        assert!(dest[8192..12288].iter().all(|&b| b == b'C'));
    }

    #[tokio::test]
    async fn test_write_pages_direct() {
        let data = vec![0u8; 64 * 1024];
        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut page_buf: PageBuffer<_, 8> = PageBuffer::new(block_dev);

        let mut src = vec![0u8; 8192];
        src[..4096].fill(b'X');
        src[4096..8192].fill(b'Y');

        let bytes_written = page_buf.write_pages_direct(1, &src, 2).await.unwrap();

        assert_eq!(bytes_written, 8192);

        let inner = page_buf.into_inner().0.into_inner().into_inner();
        assert!(inner[4096..8192].iter().all(|&b| b == b'X'));
        assert!(inner[8192..12288].iter().all(|&b| b == b'Y'));
    }

    #[tokio::test]
    async fn test_type_aliases() {
        let data = vec![0u8; 64 * 1024];
        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));

        // Test PageBuffer4K alias
        let mut page_buf: PageBuffer4K<_> = PageBuffer4K::new(block_dev);
        assert_eq!(PageBuffer4K::<TestDevice>::PAGE_SIZE, 4096);

        page_buf.read_page(0).await.unwrap();
        assert_eq!(page_buf.data().unwrap().len(), 4096);
    }

    #[tokio::test]
    async fn test_2k_pages() {
        let mut data = vec![0u8; 64 * 1024];
        data[..2048].fill(b'A');
        data[2048..4096].fill(b'B');

        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut page_buf: PageBuffer2K<_> = PageBuffer2K::new(block_dev);

        assert_eq!(PageBuffer2K::<TestDevice>::PAGE_SIZE, 2048);

        page_buf.read_page(0).await.unwrap();
        let page_data = page_buf.data().unwrap();
        assert_eq!(page_data.len(), 2048);
        assert!(page_data.iter().all(|&b| b == b'A'));

        page_buf.read_page(1).await.unwrap();
        let page_data = page_buf.data().unwrap();
        assert!(page_data.iter().all(|&b| b == b'B'));
    }
}
