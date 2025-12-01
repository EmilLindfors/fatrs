//! Stack-allocated block device adapters for the fatrs ecosystem.
//!
//! This crate provides adapters for working with block devices at various abstraction levels,
//! all using stack allocation for no_std compatibility:
//!
//! - [`BufStream`]: Wraps a block device to provide byte-level Read/Write/Seek with single-block buffering
//! - [`PageBuffer`]: Aggregates multiple small blocks into larger pages (e.g., 8×512B → 4KB)
//! - [`PageStream`]: Like BufStream but buffers entire pages for better sequential performance
//! - [`StreamSlice`]: Provides a view into a portion of a stream
//!
//! For heap-allocated large page buffers (128KB+), see `fatrs-adapters-alloc`.
//!
//! # Example: 4KB Page Buffering over 512B Blocks
//!
//! ```ignore
//! use fatrs_adapters_core::{PageBuffer, PageStream};
//!
//! // SD card with 512-byte blocks
//! let sd_card: impl BlockDevice<512> = ...;
//!
//! // Option 1: Use PageBuffer for page-level operations (stack allocated, const generic size)
//! let mut pages: PageBuffer<_, 8> = PageBuffer::new(sd_card);
//! pages.read_page(0).await?;
//! let data = pages.data().unwrap();
//!
//! // Option 2: Use PageStream for byte-level access with page buffering
//! let mut stream: PageStream<_, 8> = PageStream::new(sd_card);
//! stream.read(&mut buffer).await?;  // Buffers 4KB internally
//! ```

#![cfg_attr(not(test), no_std)]

/// Macro to define adapter error types with common boilerplate.
///
/// This macro generates:
/// - An error enum with an `Io(E)` variant for underlying errors
/// - `From<E>` implementation for automatic error conversion
/// - `Display` implementation
/// - `core::error::Error` implementation
///
/// # Example
///
/// ```ignore
/// define_adapter_error! {
///     /// Error type for BufStream operations
///     pub enum BufStreamError<E> {
///         /// Underlying I/O error
///         Io(E) => "IO error: {}",
///     }
/// }
/// ```
#[macro_export]
macro_rules! define_adapter_error {
    // Simple case: only Io variant
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident<$err:ident> {
            $(#[$io_meta:meta])*
            Io($io_ty:ident) => $io_msg:literal,
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug)]
        #[non_exhaustive]
        $vis enum $name<$err> {
            $(#[$io_meta])*
            Io($io_ty),
        }

        impl<$err> From<$err> for $name<$err> {
            fn from(e: $err) -> Self {
                Self::Io(e)
            }
        }

        impl<$err: core::fmt::Display> core::fmt::Display for $name<$err> {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                match self {
                    Self::Io(e) => write!(f, $io_msg, e),
                }
            }
        }

        impl<$err: core::fmt::Debug + core::fmt::Display> core::error::Error for $name<$err> {}
    };

    // Extended case: Io variant plus additional variants
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident<$err:ident> {
            $(#[$io_meta:meta])*
            Io($io_ty:ident) => $io_msg:literal,
            $(
                $(#[$variant_meta:meta])*
                $variant:ident $({ $($field:ident : $field_ty:ty),* $(,)? })? => $variant_msg:literal
            ),+ $(,)?
        }
    ) => {
        $(#[$meta])*
        #[derive(Debug)]
        #[non_exhaustive]
        $vis enum $name<$err> {
            $(#[$io_meta])*
            Io($io_ty),
            $(
                $(#[$variant_meta])*
                $variant $({ $($field : $field_ty),* })?,
            )+
        }

        impl<$err> From<$err> for $name<$err> {
            fn from(e: $err) -> Self {
                Self::Io(e)
            }
        }

        impl<$err: core::fmt::Display> core::fmt::Display for $name<$err> {
            fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
                match self {
                    Self::Io(e) => write!(f, $io_msg, e),
                    $(
                        Self::$variant $({ $($field),* })? => write!(f, $variant_msg $(, $($field),*)?),
                    )+
                }
            }
        }

        impl<$err: core::fmt::Debug + core::fmt::Display> core::error::Error for $name<$err> {}
    };
}

// MUST be the first module listed
mod fmt;

mod io_helpers;
pub use io_helpers::{read_exact_async, write_all_async};

mod buf_stream;
mod page_buffer;
mod page_stream;
mod stream_slice;

pub use buf_stream::{BufStream, BufStreamError};
pub use page_buffer::{
    BLOCK_SIZE, PageBuffer, PageBuffer1K, PageBuffer2K, PageBuffer4K, PageBuffer8K, PageBufferError,
};
pub use page_stream::{PageStream, PageStream2K, PageStream4K, PageStream8K, PageStreamError};
pub use stream_slice::{StreamSlice, StreamSliceError};
