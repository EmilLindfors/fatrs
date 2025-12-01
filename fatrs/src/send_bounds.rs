//! Conditional Send/Sync bounds for multi-threaded executor support.
//!
//! # Threading Model
//!
//! The `FileSystem` type is `Send + Sync` when the underlying IO type is `Send`.
//! This allows sharing the filesystem across threads using `Arc<FileSystem>`.
//!
//! However, the **futures** returned by async methods (e.g., `file.read()`, `dir.create_file()`)
//! are **not** `Send`. This is because `embedded-io-async` traits use `async fn` which
//! doesn't guarantee Send futures - this is intentional for embedded compatibility.
//!
//! # Concurrent Access Patterns
//!
//! For concurrent filesystem operations, use one of these approaches:
//!
//! ## Single-threaded runtime (recommended for this crate)
//! ```ignore
//! #[tokio::main(flavor = "current_thread")]
//! async fn main() {
//!     // All tasks run on the same thread, no Send requirement
//! }
//! ```
//!
//! ## LocalSet with multi-threaded runtime
//! ```ignore
//! use tokio::task::{LocalSet, spawn_local};
//!
//! let local = LocalSet::new();
//! local.run_until(async {
//!     let fs = Arc::new(FileSystem::new(device, FsOptions::new()).await?);
//!
//!     // spawn_local doesn't require Send
//!     spawn_local(async move {
//!         let root = fs.root_dir();
//!         // ... filesystem operations
//!     });
//! }).await;
//! ```
//!
//! ## Embassy and other embedded executors
//! Single-threaded by design, works naturally with this crate.
//!
//! # Why not `tokio::spawn`?
//!
//! `tokio::spawn` requires `Send` futures because tasks may be moved between
//! worker threads. Since `embedded-io-async` is designed for embedded systems
//! where single-threaded executors are the norm, its futures are not `Send`.
//!
//! If you need `tokio::spawn` specifically, consider using tokio's native
//! `AsyncRead`/`AsyncWrite` traits with a different FAT implementation.

/// Marker trait for Send bounds when `send` feature is enabled.
#[cfg(feature = "send")]
pub trait MaybeSend: Send {}
#[cfg(feature = "send")]
impl<T: Send> MaybeSend for T {}

#[cfg(not(feature = "send"))]
pub trait MaybeSend {}
#[cfg(not(feature = "send"))]
impl<T> MaybeSend for T {}

/// Marker trait for Sync bounds when `send` feature is enabled.
#[cfg(feature = "send")]
pub trait MaybeSync: Sync {}
#[cfg(feature = "send")]
impl<T: Sync> MaybeSync for T {}

#[cfg(not(feature = "send"))]
pub trait MaybeSync {}
#[cfg(not(feature = "send"))]
impl<T> MaybeSync for T {}

/// Marker trait for Send + Sync bounds when `send` feature is enabled.
#[cfg(feature = "send")]
pub trait MaybeSendSync: Send + Sync {}
#[cfg(feature = "send")]
impl<T: Send + Sync> MaybeSendSync for T {}

#[cfg(not(feature = "send"))]
pub trait MaybeSendSync {}
#[cfg(not(feature = "send"))]
impl<T> MaybeSendSync for T {}
