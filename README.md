# fatrs

[![CI Status](https://github.com/EmilLindfors/fatrs/actions/workflows/ci.yml/badge.svg)](https://github.com/EmilLindfors/fatrs/actions/workflows/ci.yml)
[![MIT licensed](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE.txt)
[![crates.io](https://img.shields.io/crates/v/fatrs)](https://crates.io/crates/fatrs)
[![Documentation](https://docs.rs/fatrs/badge.svg)](https://docs.rs/fatrs)

![Minimum rustc version](https://img.shields.io/badge/rustc-1.85+-green.svg)

A high-performance, async-first FAT filesystem implementation for Rust, designed for both embedded systems and desktop applications. Achieves **10-100x performance improvements** over baseline FAT implementations through advanced caching and optimization techniques.

## Crate Ecosystem

fatrs uses a **hexagonal architecture** with clean separation between domain logic, ports, and adapters:

| Crate | Layer | Description | Features |
|-------|-------|-------------|----------|
| [`fatrs`](fatrs/) | **Domain** | Core FAT12/16/32 filesystem | Async-first, `no_std` compatible, performance optimizations |
| [`fatrs-block-device`](fatrs-block-device/) | **Port** | `BlockDevice<SIZE>` trait | DMA alignment, Send variant, storage abstraction |
| [`fatrs-adapters-core`](fatrs-adapters-core/) | **Adapter** | Stack-allocated adapters | `no_std`, BufStream, PageBuffer, PageStream |
| [`fatrs-adapters-alloc`](fatrs-adapters-alloc/) | **Adapter** | Heap-allocated adapters | Large page buffers (128KB+) for SSDs |
| [`fatrs-block-platform`](fatrs-block-platform/) | **Adapter** | Platform-specific implementations | Windows, Linux, macOS, SPI SD cards |
| [`fatrs-cli`](fatrs-cli/) | **Application** | CLI tools | `fatrs`, `fatrs-tui` file browser |
| [`fatrs-fuse`](fatrs-fuse/) | **Application** | FUSE filesystem mount | `fatrs-mount` for Linux/macOS/Windows |

## Key Features

### Core Functionality
- Full **FAT12/16/32 support** with automatic type detection
- **Async-first design** for Embassy, RTIC, tokio, and other async frameworks
- **no_std compatible** with optional `alloc` support
- **Long File Name (LFN)** support
- Comprehensive file and directory operations

### Performance Optimizations
- **FAT Sector Cache** (4KB-16KB): 10-50x faster random access
- **Multi-Cluster Batched I/O**: 2-5x sequential throughput, 16x less flash wear
- **Free Cluster Bitmap**: O(1) allocation instead of O(n) FAT scanning
- **Directory Entry Cache**: 3-5x faster nested directory access

### Safety Features
- **Transaction-safe mode**: Power-loss resilience with two-phase commit (feature: `transaction-safe`)
- **File locking**: Concurrent access protection with shared/exclusive locks (feature: `file-locking`)
- **Send bounds**: Multi-threaded executor support (feature: `send`)
- **Dirty file panic**: Debug mode to catch unflushed files (feature: `dirty-file-panic`)

## Quick Start

### Desktop (tokio)

```rust
use fatrs::{FileSystem, FsOptions};
use embedded_io_async::Write;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let img_file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("fat32.img")
        .await?;

    let fs = FileSystem::new(img_file, FsOptions::new()).await?;
    let root_dir = fs.root_dir();

    // Create and write to a file
    let mut file = root_dir.create_file("hello.txt").await?;
    file.write_all(b"Hello, fatrs!").await?;
    file.flush().await?;

    fs.flush().await?;
    Ok(())
}
```

### Embedded (Embassy)

```rust
use fatrs::{FileSystem, FsOptions};
use fatrs_adapters_core::BufStream;
use embedded_io_async::Write;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let sd_card = init_sd_card().await;
    let buf_stream = BufStream::<_, 512>::new(sd_card);

    let fs = FileSystem::new(buf_stream, FsOptions::new()).await.unwrap();

    let mut file = fs.root_dir().create_file("test.log").await.unwrap();
    file.write_all(b"Hello from embedded!").await.unwrap();
    file.flush().await.unwrap();

    fs.unmount().await.unwrap();
}
```

## Feature Presets

### Desktop (all optimizations)
```toml
[dependencies]
fatrs = { version = "0.3", features = ["desktop"] }
```
Includes: `std`, `alloc`, `lfn`, `unicode`, `log`, `time-provider`, `fat-cache`, `multi-cluster-io`, `cluster-bitmap-medium`, `file-locking`, `send`

**Use case**: Desktop applications, servers, high-performance scenarios with ample RAM

### Embedded (no_std optimized)
```toml
[dependencies]
fatrs = { version = "0.3", default-features = false, features = ["embedded"] }
```
Includes: `lfn`, `fat-cache`, `multi-cluster-io`

**Use case**: ESP32, STM32, RP2040 microcontrollers with 64KB+ RAM

### Minimal (ultra-constrained)
```toml
[dependencies]
fatrs = { version = "0.3", default-features = false, features = ["lfn"] }
```

**Use case**: Resource-constrained microcontrollers with <16KB RAM

### Safety-Critical
```toml
[dependencies]
fatrs = { version = "0.3", features = ["std", "transaction-safe", "file-locking", "dirty-file-panic"] }
```

**Use case**: Medical, automotive, aerospace applications requiring power-loss resilience

## CLI Tools

### fatrs - Command-line utility (`fatrs-cli` crate)
```bash
# List files
fatrs ls image.img

# Copy files to/from image
fatrs cp image.img :path/in/image ./local/path

# Create FAT32 image
fatrs create -s 128M -t 32 new_image.img

# Show filesystem info
fatrs info image.img
```

### fatrs-tui - Terminal file browser (`fatrs-cli` crate)
Interactive TUI for browsing FAT images with vim-like keybindings
```bash
fatrs-tui image.img
```

### fatrs-mount - FUSE filesystem (`fatrs-fuse` crate)
Mount FAT images as native filesystems on Linux, macOS, and Windows (via WinFsp)
```bash
# Mount with transaction safety
fatrs-mount image.img /mnt/fatfs --transaction-safe

# Unmount (Linux/macOS)
fusermount -u /mnt/fatfs
```

## Performance

| Operation | Baseline | Optimized | Improvement |
|-----------|----------|-----------|-------------|
| Sequential Read | 750 KB/s | 3-5 MB/s | **4-6x faster** |
| Random Access | 500ms avg | 10-20ms | **25-50x faster** |
| Allocation (90% full) | 2000ms | 5-10ms | **200-400x faster** |
| Flash Wear | Baseline | Reduced | **16x less wear** |

## Compatibility

- **Rust:** 1.85+ (Rust 2024 Edition)
- **Async Runtime:** Any (tokio, embassy, async-std)
- **Architecture:** All platforms (x86, ARM, RISC-V)
- **OS:** std and no_std

## Architecture

fatrs follows **hexagonal architecture** (ports and adapters pattern) for maximum flexibility:

```
┌─────────────────────────────────────────────────────────┐
│                    Applications                         │
│  (fatrs-cli, fatrs-fuse, embedded apps)                │
└────────────────────┬───────────────────────────────────┘
                     │
┌────────────────────▼───────────────────────────────────┐
│                  Domain Core                            │
│                   (fatrs)                               │
│  • FAT12/16/32 logic     • Performance optimizations   │
│  • File operations       • Transaction safety          │
│  • Directory operations  • File locking                │
└────────────────────┬───────────────────────────────────┘
                     │ BlockDevice<SIZE> port
┌────────────────────▼───────────────────────────────────┐
│                   Adapters                              │
│  ┌──────────────────┬──────────────────┬──────────────┐│
│  │ fatrs-adapters-  │ fatrs-adapters-  │ fatrs-block- ││
│  │ core (no_std)    │ alloc            │ platform     ││
│  │ • BufStream      │ • LargePageBuf   │ • Windows    ││
│  │ • PageBuffer     │ • SSD optimized  │ • Linux      ││
│  │ • PageStream     │                  │ • macOS      ││
│  │                  │                  │ • SPI SD     ││
│  └──────────────────┴──────────────────┴──────────────┘│
└─────────────────────────────────────────────────────────┘
```

**Benefits**:
- **Testability**: Domain logic isolated from I/O
- **Portability**: Same core runs on embedded and desktop
- **Flexibility**: Swap storage backends without changing filesystem code
- **Performance**: Platform-specific optimizations in adapters

## Documentation

- **[ARCHITECTURE.md](ARCHITECTURE.md)** - Detailed architecture and optimization techniques
- **[TODO.md](TODO.md)** - Roadmap and planned features
- **[CHANGELOG.md](CHANGELOG.md)** - Version history
- **[API Documentation](https://docs.rs/fatrs)** - Full API reference

## Origin and Acknowledgments

fatrs is a substantial evolution of FAT filesystem implementations in Rust. The project builds upon foundational work from:

- **[rust-fatfs](https://github.com/rafalh/rust-fatfs)** by Rafal Harabien - The original pure-Rust FAT implementation that pioneered no_std FAT support
- **[embedded-fatfs](https://github.com/mabezdev/embedded-fatfs)** by Scott Mabin - Async adaptation for embedded systems using embedded-hal

fatrs represents a significant rewrite and expansion beyond these predecessors:

- **Complete async-first redesign** using Rust 2024 Edition features
- **Novel performance optimizations** including FAT sector caching, multi-cluster batched I/O, and free cluster bitmaps
- **Hexagonal architecture** with clean separation between the core filesystem and I/O adapters
- **Comprehensive tooling** including CLI, TUI browser, and FUSE mount support
- **Safety features** like transaction-safe writes and file locking

We're grateful to the original authors for their foundational work that made this project possible.

## Research & References

Performance optimizations based on:
- ChaN FatFs application notes
- exFAT specification and Linux driver
- PX5 FILE system (2024) caching strategies

See [ARCHITECTURE.md](ARCHITECTURE.md#research-references) for complete bibliography.

## Contributing

Contributions welcome! Please:
1. Fork the repository
2. Create a feature branch
3. Run `cargo fmt` and `cargo clippy`
4. Submit a pull request

## License

Licensed under MIT license ([LICENSE.txt](LICENSE.txt) or http://opensource.org/licenses/MIT)

Unless you explicitly state otherwise, any contribution intentionally submitted for inclusion in the work by you shall be licensed as above.
