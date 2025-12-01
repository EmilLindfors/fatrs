//! Heap-allocated block device adapters for the fatrs ecosystem.
//!
//! This crate provides adapters for working with block devices using heap allocation,
//! enabling large page sizes (128KB+) that would be impractical with stack allocation:
//!
//! - [`LargePageBuffer`]: Runtime-sized page buffer backed by Vec (128KB+ pages for SSDs)
//! - [`LargePageStream`]: Byte-level Read/Write/Seek over LargePageBuffer
//!
//! For stack-allocated adapters (no_std compatible), see `fatrs-adapters-core`.
//!
//! # Example: 128KB Page Buffering for SSDs
//!
//! ```ignore
//! use fatrs_adapters_alloc::{LargePageBuffer, LargePageStream, presets};
//!
//! // SSD with optimal 128KB I/O size
//! let ssd: impl BlockDevice<512> = ...;
//!
//! // Option 1: Use LargePageBuffer for page-level operations
//! let mut buffer = LargePageBuffer::new(ssd, presets::PAGE_128K);
//! buffer.read_page(0).await?;
//!
//! // Option 2: Use LargePageStream for byte-level access
//! let mut stream = LargePageStream::new(ssd, presets::PAGE_128K);
//! stream.read(&mut buf).await?;  // Buffers 128KB internally
//! ```

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod large_page_buffer;
mod large_page_stream;

pub use large_page_buffer::{LargePageBuffer, LargePageBufferError, presets};
pub use large_page_stream::{LargePageStream, LargePageStreamError};
