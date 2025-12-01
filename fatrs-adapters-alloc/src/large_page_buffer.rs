//! Large page buffering for block devices (heap-allocated)
//!
//! This module provides `LargePageBuffer` which uses heap allocation to support
//! arbitrarily large page sizes (e.g., 128KB, 1MB) that would be impractical
//! with stack allocation.
//!
//! # Use Cases
//!
//! - **SSDs**: Benefit from 128KB+ I/O sizes for optimal throughput
//! - **NVMe**: Often optimal at 128KB-512KB transfer sizes
//! - **Large sequential transfers**: Reduce syscall overhead
//!
//! # Example
//!
//! ```ignore
//! use fatrs_adapters_alloc::LargePageBuffer;
//!
//! // Create a 128KB page buffer for SSD optimization
//! let mut buffer = LargePageBuffer::new(ssd_device, 128 * 1024);
//!
//! // Read page 0 (128KB)
//! buffer.read_page(0).await?;
//! let data = buffer.data().unwrap();
//! ```

extern crate alloc;

use alloc::vec::Vec;

use aligned::Aligned;
use fatrs_block_device::BlockDevice;
use fatrs_adapters_core::BLOCK_SIZE;

/// Error type for LargePageBuffer operations
#[derive(Clone, Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum LargePageBufferError<E> {
    /// Underlying block device error
    Io(E),
    /// Buffer contains uncommitted writes for a different page
    DirtyPageConflict {
        /// The page currently in the buffer
        current_page: u32,
        /// The page that was requested
        requested_page: u32,
    },
    /// No page is currently loaded
    NoPageLoaded,
    /// Invalid page size (must be multiple of block size)
    InvalidPageSize {
        page_size: usize,
        block_size: usize,
    },
}

impl<E: core::fmt::Display> core::fmt::Display for LargePageBufferError<E> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            LargePageBufferError::Io(e) => write!(f, "IO error: {}", e),
            LargePageBufferError::DirtyPageConflict {
                current_page,
                requested_page,
            } => write!(
                f,
                "dirty page conflict: buffer has page {} but page {} requested",
                current_page, requested_page
            ),
            LargePageBufferError::NoPageLoaded => write!(f, "no page loaded"),
            LargePageBufferError::InvalidPageSize {
                page_size,
                block_size,
            } => write!(
                f,
                "invalid page size: {} is not a multiple of block size {}",
                page_size, block_size
            ),
        }
    }
}

impl<E: core::fmt::Debug + core::fmt::Display> core::error::Error for LargePageBufferError<E> {}

/// Large page buffer with heap-allocated storage
///
/// Unlike `PageBuffer` which uses stack allocation with const generics,
/// `LargePageBuffer` uses heap allocation allowing runtime-configurable
/// page sizes up to any practical limit.
///
/// # Recommended Page Sizes
///
/// | Storage Type | Recommended Size | Rationale |
/// |--------------|------------------|-----------|
/// | SD Card | 4-32 KB | Matches internal page/erase size |
/// | SATA SSD | 128 KB | Optimal for AHCI command overhead |
/// | NVMe SSD | 128-512 KB | Matches NVMe queue depth sweet spot |
/// | HDD | 1 MB+ | Amortizes seek time |
///
/// # Memory Usage
///
/// The buffer allocates `page_size` bytes on the heap. For a 128KB page,
/// this is 128KB of heap memory per `LargePageBuffer` instance.
pub struct LargePageBuffer<D: BlockDevice<BLOCK_SIZE>> {
    /// The underlying block device
    inner: D,

    /// Heap-allocated buffer for page data (aligned blocks for efficient I/O)
    buffer: Vec<Aligned<D::Align, [u8; BLOCK_SIZE]>>,

    /// Page size in bytes
    page_size: usize,

    /// Number of blocks per page
    blocks_per_page: usize,

    /// Whether the buffer contains uncommitted writes
    dirty: bool,

    /// The current page number loaded in the buffer (if any)
    current_page: Option<u32>,
}

impl<D: BlockDevice<BLOCK_SIZE>> LargePageBuffer<D> {
    /// Create a new large page buffer with the specified page size
    ///
    /// # Arguments
    /// * `inner` - The underlying block device
    /// * `page_size` - Page size in bytes (must be multiple of 512)
    ///
    /// # Panics
    /// Panics if `page_size` is not a multiple of `BLOCK_SIZE` (512).
    ///
    /// # Example
    /// ```ignore
    /// // 128KB pages for SSD
    /// let buffer = LargePageBuffer::new(device, 128 * 1024);
    ///
    /// // 1MB pages for HDD sequential access
    /// let buffer = LargePageBuffer::new(device, 1024 * 1024);
    /// ```
    pub fn new(inner: D, page_size: usize) -> Self {
        assert!(
            page_size % BLOCK_SIZE == 0,
            "page_size must be a multiple of BLOCK_SIZE (512)"
        );
        assert!(page_size >= BLOCK_SIZE, "page_size must be >= BLOCK_SIZE");

        let blocks_per_page = page_size / BLOCK_SIZE;

        // Create aligned blocks for efficient I/O
        let buffer: Vec<Aligned<D::Align, [u8; BLOCK_SIZE]>> =
            (0..blocks_per_page).map(|_| Aligned([0u8; BLOCK_SIZE])).collect();

        Self {
            inner,
            buffer,
            page_size,
            blocks_per_page,
            dirty: false,
            current_page: None,
        }
    }

    /// Create with validated page size (returns Result instead of panicking)
    pub fn try_new(inner: D, page_size: usize) -> Result<Self, LargePageBufferError<D::Error>> {
        if page_size % BLOCK_SIZE != 0 || page_size < BLOCK_SIZE {
            return Err(LargePageBufferError::InvalidPageSize {
                page_size,
                block_size: BLOCK_SIZE,
            });
        }

        Ok(Self::new(inner, page_size))
    }

    /// Get the page size in bytes
    pub fn page_size(&self) -> usize {
        self.page_size
    }

    /// Get the number of blocks per page
    pub fn blocks_per_page(&self) -> usize {
        self.blocks_per_page
    }

    /// Returns the inner block device, consuming this buffer
    pub fn into_inner(self) -> D {
        self.inner
    }

    /// Get a reference to the inner block device
    pub fn inner(&self) -> &D {
        &self.inner
    }

    /// Get a mutable reference to the inner block device
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
    pub fn page_to_block_address(&self, page_num: u32) -> u32 {
        page_num * self.blocks_per_page as u32
    }

    /// Convert block address to page number
    #[inline]
    pub fn block_to_page_address(&self, block_num: u32) -> u32 {
        block_num / self.blocks_per_page as u32
    }

    /// Read a page from the block device into the buffer
    pub async fn read_page(&mut self, page_num: u32) -> Result<(), LargePageBufferError<D::Error>> {
        // Check if we already have this page
        if self.current_page == Some(page_num) {
            return Ok(());
        }

        // Fail if we have a dirty page that's different
        if self.dirty {
            if let Some(current) = self.current_page {
                return Err(LargePageBufferError::DirtyPageConflict {
                    current_page: current,
                    requested_page: page_num,
                });
            }
        }

        let block_address = self.page_to_block_address(page_num);

        // Read all blocks in one call (efficient!)
        self.inner
            .read(block_address, &mut self.buffer)
            .await
            .map_err(LargePageBufferError::Io)?;

        self.current_page = Some(page_num);
        self.dirty = false;

        Ok(())
    }

    /// Write the buffer contents to a page on the block device
    pub async fn write_page(&mut self, page_num: u32) -> Result<(), LargePageBufferError<D::Error>> {
        let block_address = self.page_to_block_address(page_num);

        // Write all blocks in one call (efficient!)
        self.inner
            .write(block_address, &self.buffer)
            .await
            .map_err(LargePageBufferError::Io)?;

        self.current_page = Some(page_num);
        self.dirty = false;

        Ok(())
    }

    /// Get the buffered page data as a byte slice
    pub fn data(&self) -> Option<&[u8]> {
        if self.current_page.is_some() {
            // Safety: Aligned blocks are contiguous in memory and we know the total size
            let ptr = self.buffer.as_ptr() as *const u8;
            Some(unsafe { core::slice::from_raw_parts(ptr, self.page_size) })
        } else {
            None
        }
    }

    /// Get the buffered page data as a mutable byte slice and mark as dirty
    pub fn data_mut(&mut self) -> Option<&mut [u8]> {
        if self.current_page.is_some() {
            self.dirty = true;
            // Safety: Aligned blocks are contiguous in memory and we know the total size
            let ptr = self.buffer.as_mut_ptr() as *mut u8;
            Some(unsafe { core::slice::from_raw_parts_mut(ptr, self.page_size) })
        } else {
            None
        }
    }

    /// Copy data from the buffer to a destination slice
    pub fn copy_to(&self, dest: &mut [u8]) -> Result<usize, LargePageBufferError<D::Error>> {
        let data = self.data().ok_or(LargePageBufferError::NoPageLoaded)?;
        let len = dest.len().min(self.page_size);
        dest[..len].copy_from_slice(&data[..len]);
        Ok(len)
    }

    /// Copy data into the buffer and mark as dirty
    pub fn copy_from(&mut self, src: &[u8], page_num: u32) {
        // Get mutable byte slice view of buffer
        let ptr = self.buffer.as_mut_ptr() as *mut u8;
        let buffer_bytes = unsafe { core::slice::from_raw_parts_mut(ptr, self.page_size) };

        let len = src.len().min(self.page_size);
        buffer_bytes[..len].copy_from_slice(&src[..len]);

        // Zero the rest if src is smaller
        if len < self.page_size {
            buffer_bytes[len..].fill(0);
        }

        self.dirty = true;
        self.current_page = Some(page_num);
    }

    /// Flush the buffer if dirty
    pub async fn flush(&mut self) -> Result<(), LargePageBufferError<D::Error>> {
        if self.dirty {
            if let Some(page_num) = self.current_page {
                self.write_page(page_num).await?;
            }
        }
        Ok(())
    }

    /// Clear the buffer state without writing
    pub fn clear(&mut self) {
        self.dirty = false;
        self.current_page = None;
    }

    /// Get the size of the underlying device in pages
    pub async fn size_in_pages(&mut self) -> Result<u64, LargePageBufferError<D::Error>> {
        let bytes = self.inner.size().await.map_err(LargePageBufferError::Io)?;
        Ok(bytes / self.page_size as u64)
    }

    /// Read and modify a page atomically
    pub async fn modify_page<F>(
        &mut self,
        page_num: u32,
        modifier: F,
    ) -> Result<(), LargePageBufferError<D::Error>>
    where
        F: FnOnce(&mut [u8]) -> bool,
    {
        self.read_page(page_num).await?;

        // Get mutable byte slice view of buffer
        let ptr = self.buffer.as_mut_ptr() as *mut u8;
        let buffer_bytes = unsafe { core::slice::from_raw_parts_mut(ptr, self.page_size) };

        if modifier(buffer_bytes) {
            self.dirty = true;
        }
        Ok(())
    }

    /// Resize the page buffer
    ///
    /// This clears any cached data and reallocates the buffer.
    ///
    /// # Panics
    /// Panics if `new_page_size` is not a multiple of `BLOCK_SIZE`.
    pub fn resize(&mut self, new_page_size: usize) {
        assert!(
            new_page_size % BLOCK_SIZE == 0,
            "page_size must be a multiple of BLOCK_SIZE (512)"
        );

        let new_blocks_per_page = new_page_size / BLOCK_SIZE;
        self.buffer.resize(new_blocks_per_page, Aligned([0u8; BLOCK_SIZE]));
        self.page_size = new_page_size;
        self.blocks_per_page = new_blocks_per_page;
        self.dirty = false;
        self.current_page = None;
    }
}

/// Common page size presets
pub mod presets {
    /// 4KB - Standard OS page size, good for SD cards
    pub const PAGE_4K: usize = 4 * 1024;

    /// 8KB - Double page size
    pub const PAGE_8K: usize = 8 * 1024;

    /// 16KB - Matches some SSD internal page sizes
    pub const PAGE_16K: usize = 16 * 1024;

    /// 32KB - Good balance for mixed workloads
    pub const PAGE_32K: usize = 32 * 1024;

    /// 64KB - Traditional "large" I/O size
    pub const PAGE_64K: usize = 64 * 1024;

    /// 128KB - Optimal for many SATA SSDs
    pub const PAGE_128K: usize = 128 * 1024;

    /// 256KB - Good for NVMe sequential
    pub const PAGE_256K: usize = 256 * 1024;

    /// 512KB - Large sequential transfers
    pub const PAGE_512K: usize = 512 * 1024;

    /// 1MB - Maximum practical for most use cases
    pub const PAGE_1M: usize = 1024 * 1024;
}

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
            Ok(1024 * 1024) // 1MB test device
        }
    }

    #[tokio::test]
    async fn test_large_page_buffer_128k() {
        let mut data = vec![0u8; 1024 * 1024]; // 1MB
        // Fill first 128KB page with 'A's
        data[..presets::PAGE_128K].fill(b'A');
        // Fill second 128KB page with 'B's
        data[presets::PAGE_128K..presets::PAGE_128K * 2].fill(b'B');

        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut buffer = LargePageBuffer::new(block_dev, presets::PAGE_128K);

        assert_eq!(buffer.page_size(), 128 * 1024);
        assert_eq!(buffer.blocks_per_page(), 256);

        // Read page 0
        buffer.read_page(0).await.unwrap();
        let page_data = buffer.data().unwrap();
        assert_eq!(page_data.len(), 128 * 1024);
        assert!(page_data.iter().all(|&b| b == b'A'));

        // Read page 1
        buffer.read_page(1).await.unwrap();
        let page_data = buffer.data().unwrap();
        assert!(page_data.iter().all(|&b| b == b'B'));
    }

    #[tokio::test]
    async fn test_large_page_write() {
        let data = vec![0u8; 1024 * 1024];
        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut buffer = LargePageBuffer::new(block_dev, presets::PAGE_64K);

        // Write to page 1
        let write_data = vec![b'X'; presets::PAGE_64K];
        buffer.copy_from(&write_data, 1);
        buffer.flush().await.unwrap();

        // Verify
        let inner = buffer.into_inner().0.into_inner().into_inner();
        let page1_start = presets::PAGE_64K;
        assert!(inner[page1_start..page1_start + presets::PAGE_64K]
            .iter()
            .all(|&b| b == b'X'));
    }

    #[tokio::test]
    async fn test_resize() {
        let data = vec![0u8; 1024 * 1024];
        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));
        let mut buffer = LargePageBuffer::new(block_dev, presets::PAGE_4K);

        assert_eq!(buffer.page_size(), 4096);

        // Resize to 128KB
        buffer.resize(presets::PAGE_128K);
        assert_eq!(buffer.page_size(), 128 * 1024);
        assert_eq!(buffer.blocks_per_page(), 256);
        assert!(buffer.current_page().is_none()); // Cache cleared
    }

    #[test]
    fn test_presets() {
        assert_eq!(presets::PAGE_4K, 4 * 1024);
        assert_eq!(presets::PAGE_128K, 128 * 1024);
        assert_eq!(presets::PAGE_1M, 1024 * 1024);
    }

    #[tokio::test]
    async fn test_try_new_invalid() {
        let data = vec![0u8; 1024];
        let cursor = std::io::Cursor::new(data);
        let block_dev = TestBlockDevice(FromTokio::new(cursor));

        // Invalid: not multiple of 512
        let result = LargePageBuffer::try_new(block_dev, 1000);
        assert!(matches!(
            result,
            Err(LargePageBufferError::InvalidPageSize { .. })
        ));
    }
}
