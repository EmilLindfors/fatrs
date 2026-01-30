//! Platform-specific BlockDevice implementations for fatrs
//!
//! This crate provides `BlockDevice<512>` implementations for various platforms:
//!
//! - **Embedded (SPI)**: SD cards over SPI for microcontrollers (ARM, ESP32, RP2040, etc.)
//! - **Windows**: Direct device access via Win32 APIs (USB drives, flash cards)
//! - **Linux**: Block device access via `/dev/sdX` and ioctl
//! - **macOS**: Disk access via `/dev/diskX`
//!
//! ## Feature Flags
//!
//! - `sdspi` - SD card over SPI (embedded, `no_std`)
//! - `windows` - Windows device access (requires `std`)
//! - `linux` - Linux block device access (requires `std`)
//! - `macos` - macOS disk access (requires `std`)
//! - `logging` - Enable `log` crate integration
//! - `defmt-logging` - Enable `defmt` logging for embedded
//!
//! ## Examples
//!
//! ### Embedded SPI (no_std)
//!
//! ```ignore
//! use fatrs_block_platform::SdSpi;
//! use embedded_hal_async::spi::SpiDevice;
//!
//! let spi = /* your SPI device */;
//! let cs = /* your chip select pin */;
//! let mut sd = SdSpi::new(spi, cs);
//! sd.init().await?;
//! ```
//!
//! ### Windows
//!
//! ```ignore
//! use fatrs_block_platform::WindowsDevice;
//!
//! let device = WindowsDevice::open("D:", false).await?;
//! ```
//!
//! ### Linux
//!
//! ```ignore
//! use fatrs_block_platform::LinuxBlockDevice;
//!
//! let device = LinuxBlockDevice::open("/dev/sdb", false).await?;
//! ```

#![cfg_attr(not(feature = "std"), no_std)]
#![warn(missing_docs)]

// Logging support - needed by sdspi and when defmt-logging is enabled
// Must be declared early so macros are available to other modules
#[cfg_attr(any(feature = "defmt-logging", feature = "sdspi"), macro_use)]
#[cfg(any(feature = "defmt-logging", feature = "sdspi"))]
mod fmt;

// Re-export core types
pub use fatrs_block_device::{BlockDevice, SendBlockDevice};

// Generic stream adapter (requires embedded-io-async)
#[cfg(feature = "embedded-io-async")]
pub mod stream;
#[cfg(feature = "embedded-io-async")]
pub use stream::StreamBlockDevice;

// Embedded SPI module
#[cfg(feature = "sdspi")]
pub mod sdspi;
#[cfg(feature = "sdspi")]
pub use sdspi::{Card, Error as SdSpiError, SdSpi};

// Windows module
#[cfg(all(windows, feature = "windows"))]
pub mod windows;
#[cfg(all(windows, feature = "windows"))]
pub use windows::{AsyncWindowsDevice, DriveInfo, WindowsDevice, list_removable_drives};

// Linux module
#[cfg(all(target_os = "linux", feature = "linux"))]
pub mod linux;
#[cfg(all(target_os = "linux", feature = "linux"))]
pub use linux::{BlockDeviceInfo, LinuxBlockDevice, list_block_devices};

// macOS module
#[cfg(all(target_os = "macos", feature = "macos"))]
pub mod macos;
#[cfg(all(target_os = "macos", feature = "macos"))]
pub use macos::{DiskInfo, MacOSBlockDevice, list_disks};

// RP2040/RP2350 flash module
#[cfg(feature = "rpflash")]
pub mod rpflash;
#[cfg(feature = "rpflash")]
pub use rpflash::{Error as RpFlashError, RpFlash};
