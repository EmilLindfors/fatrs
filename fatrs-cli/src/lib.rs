//! fatrs-cli library
//!
//! Re-exports for CLI utilities.

// Re-export FUSE adapter from fatrs-fuse
#[cfg(any(feature = "unix-fuse", feature = "windows-fuse"))]
pub use fatrs_fuse::FuseAdapter;

// Re-export from fatrs-block-platform
#[cfg(windows)]
pub use fatrs_block_platform::{AsyncWindowsDevice, WindowsDevice, list_removable_drives};
