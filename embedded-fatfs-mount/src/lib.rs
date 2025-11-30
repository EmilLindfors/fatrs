//! embedded-fatfs-mount library
//!
//! This library provides FUSE mounting capabilities for embedded-fatfs,
//! enabling FAT images to be mounted with transaction-safe support.

#[cfg(unix)]
pub mod fuse_adapter;

#[cfg(unix)]
pub use fuse_adapter::FuseAdapter;
