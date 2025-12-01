//! fatrs-cli library
//!
//! This library provides FUSE mounting capabilities for fatrs,
//! enabling FAT images to be mounted with transaction-safe support.

#[cfg(all(unix, feature = "fuse"))]
pub mod fuse_adapter;

#[cfg(all(unix, feature = "fuse"))]
pub use fuse_adapter::FuseAdapter;
