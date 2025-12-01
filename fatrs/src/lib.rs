//! A FAT filesystem library implemented in Rust.
//!
//! # Usage
//!
//! This crate is [on crates.io](https://crates.io/crates/fatrs) and can be
//! used by adding `fatrs` to the dependencies in your project's `Cargo.toml`.
//!
//! ```toml
//! [dependencies]
//! fatrs = "0.3"
//! ```
//!
//! # Feature Presets
//!
//! - `embedded`: Core `no_std` features (lfn + fat-cache + multi-cluster-io)
//! - `desktop`: All features enabled (std + alloc + all optimizations)
//!
//! # Threading and Concurrency
//!
//! The `FileSystem` type is `Send + Sync` when the underlying IO type is `Send`,
//! allowing it to be shared across threads via `Arc<FileSystem>`.
//!
//! However, the futures returned by async methods are **not** `Send` because
//! `embedded-io-async` traits don't guarantee Send futures (by design, for embedded
//! compatibility). This means `tokio::spawn` cannot be used directly.
//!
//! For concurrent operations, use one of these patterns:
//!
//! - **Single-threaded runtime**: `#[tokio::main(flavor = "current_thread")]`
//! - **`LocalSet`**: Use `tokio::task::spawn_local` within a `LocalSet`
//! - **Embassy**: Works naturally (single-threaded by design)
//!
//! See the [`send_bounds`] module for detailed examples.
//!
//! # Examples
//!
//! ```rust,ignore
//! use tokio::fs;
//! use embedded_io_async::Write;
//!
//! #[tokio::main]
//! async fn main() -> anyhow::Result<()> {
//!     # fs::copy("resources/fat16.img", "tmp/fat.img").await?;
//!     // Initialize a filesystem object
//!     let img_file = fs::OpenOptions::new().read(true).write(true)
//!         .open("tmp/fat.img").await?;
//!     let buf_stream = tokio::io::BufStream::new(img_file);
//!     let fs = fatrs::FileSystem::new(buf_stream, fatrs::FsOptions::new()).await?;
//!     let root_dir = fs.root_dir();
//!
//!     // Write a file
//!     root_dir.create_dir("foo").await?;
//!     let mut file = root_dir.create_file("foo/hello.txt").await?;
//!     file.truncate().await?;
//!     file.write_all(b"Hello World!").await?;
//!     file.flush().await?;
//!
//!     // Read a directory
//!     let dir = root_dir.open_dir("foo").await?;
//!     let mut iter = dir.iter();
//!     while let Some(r) = iter.next().await {
//!         let entry = r?;
//!         println!("{}", entry.file_name());
//!     }
//!     # fs::remove_file("tmp/fat.img").await?;
//!     # Ok(())
//! }
//! ```

#![crate_type = "lib"]
#![crate_name = "fatrs"]
#![cfg_attr(not(feature = "std"), no_std)]
// Disable warnings to not clutter code with cfg too much
#![cfg_attr(
    not(all(feature = "alloc", feature = "lfn")),
    allow(dead_code, unused_imports)
)]
#![warn(clippy::pedantic)]
// #![warn(missing_docs)]
#![allow(
    clippy::module_name_repetitions,
    clippy::cast_possible_truncation,
    clippy::bool_to_int_with_if, // less readable
    clippy::uninlined_format_args, // not supported before Rust 1.58.0
)]

#[cfg(all(not(feature = "std"), feature = "alloc"))]
extern crate alloc;

// MUST be the first module listed
mod fmt;

mod send_bounds;
pub use send_bounds::{MaybeSend, MaybeSendSync, MaybeSync};

mod boot_sector;
mod dir;
mod dir_entry;
mod error;
mod file;
mod fs;
mod io;
mod table;
mod time;

#[cfg(feature = "fat-cache")]
mod fat_cache;

#[cfg(feature = "multi-cluster-io")]
mod multi_cluster_io;

#[cfg(feature = "dir-cache")]
mod dir_cache;

#[cfg(feature = "cluster-bitmap")]
mod cluster_bitmap;

#[cfg(feature = "transaction-safe")]
mod transaction;

#[cfg(feature = "file-locking")]
mod file_locking;

pub use crate::dir::*;
pub use crate::dir_entry::*;
pub use crate::error::*;
pub use crate::file::*;
pub use crate::fs::*;
pub use crate::time::*;

#[cfg(feature = "transaction-safe")]
pub use crate::transaction::{
    TransactionEntry, TransactionLog, TransactionState, TransactionStatistics, TransactionType,
};

#[cfg(feature = "file-locking")]
pub use crate::file_locking::{FileLockManager, FileLockState, LockType};
