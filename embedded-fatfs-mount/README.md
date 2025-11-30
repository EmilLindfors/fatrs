# embedded-fatfs-mount

FUSE mount tool for [embedded-fatfs](https://github.com/mabezdev/embedded-fatfs) with transaction-safe support.

## Features

- 🔒 **Transaction-Safe**: Mount FAT images with power-loss resilience
- 🐧 **Linux Support**: Full FUSE integration via fuser
- 🍎 **macOS Support**: Works with macFUSE
- 📁 **Standard FAT**: Compatible with FAT12, FAT16, and FAT32
- ⚡ **Pure Rust**: No C dependencies except libfuse

## Installation

### Prerequisites

**Linux:**
```bash
# Debian/Ubuntu
sudo apt install fuse3 libfuse3-dev

# Fedora/CentOS
sudo dnf install fuse3-devel

# Arch
sudo pacman -S fuse3
```

**macOS:**
```bash
brew install macfuse
```

### Build from Source

```bash
cargo build --release
```

The binary will be at `target/release/embedded-fatfs-mount`.

## Usage

### Basic Mount

```bash
# Create a mount point
mkdir /mnt/fatfs

# Mount the image
embedded-fatfs-mount image.img /mnt/fatfs

# Use the filesystem
ls /mnt/fatfs
cp file.txt /mnt/fatfs/

# Unmount (in another terminal)
fusermount -u /mnt/fatfs  # Linux
umount /mnt/fatfs          # macOS
```

### Transaction-Safe Mount

For images formatted with transaction log support:

```bash
# Mount with transaction safety
embedded-fatfs-mount image.img /mnt/fatfs --transaction-safe

# All write operations are now power-loss safe!
```

### Read-Only Mount

```bash
embedded-fatfs-mount image.img /mnt/fatfs --read-only
```

### Verbose Logging

```bash
embedded-fatfs-mount image.img /mnt/fatfs --verbose
```

## Command-Line Options

```
Usage: embedded-fatfs-mount [OPTIONS] <IMAGE> <MOUNTPOINT>

Arguments:
  <IMAGE>       Path to FAT filesystem image
  <MOUNTPOINT>  Mount point directory

Options:
  -t, --transaction-safe  Enable transaction-safe mode
  -v, --verbose          Enable verbose logging
  -r, --read-only        Mount as read-only
      --allow-other      Allow other users to access the mount
  -h, --help             Print help
  -V, --version          Print version
```

## Creating Transaction-Safe Images

Use the embedded-fatfs library to create images with transaction log support:

```bash
# Create a 10MB image with transaction log
cargo run --example create_txn_safe_image --features transaction-safe
```

Or programmatically:

```rust
use embedded_fatfs::{format_volume, FormatVolumeOptions, FatType};

let options = FormatVolumeOptions::new()
    .fat_type(FatType::Fat16)
    .with_transaction_log();  // Adds 4 reserved sectors

format_volume(&mut file, options).await?;
```

## Architecture

```
┌─────────────────────┐
│  User Applications  │
└──────────┬──────────┘
           │
    ┌──────▼──────┐
    │  FUSE/OS    │
    └──────┬──────┘
           │
    ┌──────▼──────────────┐
    │  fuser (Rust FUSE)  │
    └──────┬──────────────┘
           │
    ┌──────▼──────────────────┐
    │  FuseAdapter            │
    │  (this crate)           │
    └──────┬──────────────────┘
           │
    ┌──────▼──────────────────┐
    │  embedded-fatfs         │
    │  + transaction-safe     │
    └──────┬──────────────────┘
           │
    ┌──────▼──────┐
    │  FAT Image  │
    └─────────────┘
```

## Current Status

✅ **Complete** - Full read/write implementation with transaction safety!

### Implemented Features
- [x] CLI argument parsing with clap
- [x] FAT filesystem mounting (FAT12/16/32)
- [x] Full FUSE adapter implementation
- [x] Transaction log integration
- [x] Comprehensive error handling
- [x] Inode-to-path mapping (HashMap-based)
- [x] Async/sync bridge (tokio runtime)
- [x] Accurate timestamp conversion (chrono)

### Read Operations
- [x] `lookup()` - File/directory name resolution
- [x] `getattr()` - File attributes and metadata
- [x] `readdir()` - Directory listing with pagination
- [x] `read()` - File reading with offset support

### Write Operations
- [x] `write()` - File writing with auto-extension
- [x] `create()` - File creation
- [x] `mkdir()` - Directory creation
- [x] `unlink()` - File deletion
- [x] `rmdir()` - Directory deletion
- [x] `rename()` - File/directory rename and move
- [x] `setattr()` - File truncation/extension

### Transaction Safety
All write operations are transaction-protected when `--transaction-safe` is enabled:
- ✅ Atomic metadata updates
- ✅ Power-loss resilience
- ✅ Automatic recovery on mount
- ✅ Rollback on incomplete operations

## Supported Operations

| Operation | Command Example | Status |
|-----------|----------------|--------|
| List files | `ls /mnt/fatfs` | ✅ Working |
| Read files | `cat /mnt/fatfs/file.txt` | ✅ Working |
| Create files | `touch /mnt/fatfs/new.txt` | ✅ Working |
| Write files | `echo "data" > /mnt/fatfs/file.txt` | ✅ Working |
| Delete files | `rm /mnt/fatfs/file.txt` | ✅ Working |
| Create dirs | `mkdir /mnt/fatfs/dir` | ✅ Working |
| Delete dirs | `rmdir /mnt/fatfs/dir` | ✅ Working |
| Rename | `mv /mnt/fatfs/old /mnt/fatfs/new` | ✅ Working |
| Copy out | `cp /mnt/fatfs/file /tmp/` | ✅ Working |
| Copy in | `cp /tmp/file /mnt/fatfs/` | ✅ Working |
| Truncate | `truncate -s 100 /mnt/fatfs/file` | ✅ Working |

## Limitations

1. **Unix-only**: Currently only works on Linux/macOS/BSD (Windows via WinFsp planned)
2. **No caching**: Each operation hits the FAT image directly (good for consistency)
3. **Permissions**: FAT doesn't support Unix permissions (uses defaults: 0755/0644)
4. **Ownership**: FAT doesn't support ownership (uses uid/gid 1000)

## Performance Considerations

FUSE adds ~10-20% overhead compared to kernel drivers, but:
- Good enough for most embedded use cases
- SD card I/O is usually the bottleneck
- Transaction safety adds minimal overhead

## Testing

### Quick Test
```bash
# Build
cargo build --release

# Create test mount point
mkdir -p /tmp/fatfs-test

# Mount existing test image
./target/release/embedded-fatfs-mount fat16_txn_safe.img /tmp/fatfs-test --transaction-safe --verbose

# In another terminal, test operations:
ls -la /tmp/fatfs-test
echo "Hello FUSE!" > /tmp/fatfs-test/test.txt
cat /tmp/fatfs-test/test.txt
mkdir /tmp/fatfs-test/testdir
mv /tmp/fatfs-test/test.txt /tmp/fatfs-test/testdir/
tree /tmp/fatfs-test

# Unmount
fusermount -u /tmp/fatfs-test  # Linux
umount /tmp/fatfs-test          # macOS
```

## Contributing

Contributions welcome! Priority areas:
- Integration test suite
- Performance benchmarks
- Windows WinFsp support
- Metadata caching layer
- File handle caching
- Documentation improvements

## Troubleshooting

### "fusermount: command not found"
Install FUSE userspace tools:
```bash
# Debian/Ubuntu
sudo apt install fuse3

# macOS
brew install macfuse
```

### "Permission denied" when mounting
Ensure mount point exists and you have permissions:
```bash
mkdir -p /tmp/fatfs-test
# Or use sudo for system mount points
```

### Filesystem appears empty
Check verbose output for errors:
```bash
embedded-fatfs-mount image.img /mnt/point --verbose
```

### Write operations fail
Ensure not mounted as read-only:
```bash
# Remove --read-only flag
embedded-fatfs-mount image.img /mnt/point --transaction-safe
```

## License

MIT License - see LICENSE file

## See Also

- [embedded-fatfs](../embedded-fatfs) - The underlying FAT library
- [TRANSACTION_SAFETY.md](../TRANSACTION_SAFETY.md) - Transaction safety documentation
- [fuser](https://github.com/cberner/fuser) - Rust FUSE library
