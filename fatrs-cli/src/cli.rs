//! FAT Filesystem CLI Tool
//!
//! Command-line interface for FAT filesystem operations.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use clap::{Parser, Subcommand, ValueEnum};
use embedded_io_adapters::tokio_1::FromTokio;
use fatrs::{FatType, FormatVolumeOptions, FsOptions};
use fatrs_adapters_alloc::{LargePageStream, presets};

use crate::block_device::StreamBlockDevice;

/// Page size presets for I/O buffering
#[derive(Copy, Clone, Debug, Default, ValueEnum)]
pub enum PageSize {
    /// No buffering (direct I/O)
    #[default]
    None,
    /// 4KB pages (good for HDDs)
    #[value(name = "4k")]
    Page4K,
    /// 32KB pages
    #[value(name = "32k")]
    Page32K,
    /// 64KB pages
    #[value(name = "64k")]
    Page64K,
    /// 128KB pages (optimal for SSDs)
    #[value(name = "128k")]
    Page128K,
    /// 256KB pages
    #[value(name = "256k")]
    Page256K,
    /// 512KB pages
    #[value(name = "512k")]
    Page512K,
    /// 1MB pages (maximum throughput)
    #[value(name = "1m")]
    Page1M,
}

impl PageSize {
    /// Convert to byte size
    pub fn to_bytes(self) -> Option<usize> {
        match self {
            PageSize::None => None,
            PageSize::Page4K => Some(presets::PAGE_4K),
            PageSize::Page32K => Some(presets::PAGE_32K),
            PageSize::Page64K => Some(presets::PAGE_64K),
            PageSize::Page128K => Some(presets::PAGE_128K),
            PageSize::Page256K => Some(presets::PAGE_256K),
            PageSize::Page512K => Some(presets::PAGE_512K),
            PageSize::Page1M => Some(presets::PAGE_1M),
        }
    }
}

/// FAT Filesystem CLI Tool
#[derive(Parser, Debug)]
#[command(author, version, about = "CLI tool for FAT filesystem operations")]
pub struct Cli {
    /// Page buffer size for I/O operations
    ///
    /// Larger page sizes improve performance on SSDs by reducing
    /// command overhead. Use 128k for SSDs, 4k for HDDs.
    #[arg(short = 'b', long, global = true, default_value = "none")]
    pub page_size: PageSize,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    /// List files and directories in a FAT image
    Ls {
        /// Path to FAT filesystem image
        image: PathBuf,

        /// Path within the filesystem (default: root)
        #[arg(default_value = "/")]
        path: String,

        /// Show detailed information (long format)
        #[arg(short, long)]
        long: bool,

        /// Recursively list all files
        #[arg(short = 'R', long)]
        recursive: bool,
    },

    /// Display filesystem information
    Info {
        /// Path to FAT filesystem image
        image: PathBuf,
    },

    /// Display contents of a file
    Cat {
        /// Path to FAT filesystem image
        image: PathBuf,

        /// Path to file within the filesystem
        path: String,
    },

    /// Copy files to/from a FAT image
    Cp {
        /// Path to FAT filesystem image
        image: PathBuf,

        /// Source path (prefix with : for paths inside the image)
        source: String,

        /// Destination path (prefix with : for paths inside the image)
        dest: String,

        /// Recursively copy directories
        #[arg(short, long)]
        recursive: bool,
    },

    /// Create a directory in a FAT image
    Mkdir {
        /// Path to FAT filesystem image
        image: PathBuf,

        /// Path to create
        path: String,

        /// Create parent directories as needed
        #[arg(short, long)]
        parents: bool,
    },

    /// Remove a file or directory from a FAT image
    Rm {
        /// Path to FAT filesystem image
        image: PathBuf,

        /// Path to remove
        path: String,

        /// Recursively remove directories
        #[arg(short, long)]
        recursive: bool,
    },

    /// Create a new FAT filesystem image
    Create {
        /// Path for the new FAT image
        image: PathBuf,

        /// Size of the image (e.g., 32M, 1G)
        #[arg(short, long)]
        size: String,

        /// FAT type (12, 16, or 32)
        #[arg(short = 't', long, default_value = "32")]
        fat_type: u8,

        /// Volume label
        #[arg(short, long)]
        label: Option<String>,

        /// Source directory to copy into the image
        #[arg(short, long)]
        from: Option<PathBuf>,
    },

    /// Extract all files from a FAT image to a directory
    Extract {
        /// Path to FAT filesystem image
        image: PathBuf,

        /// Destination directory
        dest: PathBuf,
    },
}

/// Parse size string like "32M", "1G", "512K"
fn parse_size(s: &str) -> Result<u64> {
    let s = s.trim().to_uppercase();
    let (num_str, multiplier) = if s.ends_with('K') {
        (&s[..s.len() - 1], 1024u64)
    } else if s.ends_with('M') {
        (&s[..s.len() - 1], 1024 * 1024)
    } else if s.ends_with('G') {
        (&s[..s.len() - 1], 1024 * 1024 * 1024)
    } else {
        (s.as_str(), 1u64)
    };

    let num: u64 = num_str.parse().context("Invalid size number")?;
    Ok(num * multiplier)
}

/// Format file size for display
fn format_size(size: u64) -> String {
    if size < 1024 {
        format!("{:>6}", size)
    } else if size < 1024 * 1024 {
        format!("{:>5.1}K", size as f64 / 1024.0)
    } else if size < 1024 * 1024 * 1024 {
        format!("{:>5.1}M", size as f64 / (1024.0 * 1024.0))
    } else {
        format!("{:>5.1}G", size as f64 / (1024.0 * 1024.0 * 1024.0))
    }
}

/// Get the effective page size in bytes (default to 4KB for minimal buffering)
fn effective_page_size(page_size: PageSize) -> usize {
    page_size.to_bytes().unwrap_or(presets::PAGE_4K)
}

/// Open a FAT filesystem image with large page buffering
async fn open_fs_buffered(
    image: &Path,
    writable: bool,
    page_size: usize,
) -> Result<(
    fatrs::FileSystem<
        LargePageStream<StreamBlockDevice<FromTokio<tokio::fs::File>>>,
        fatrs::DefaultTimeProvider,
        fatrs::LossyOemCpConverter,
    >,
    usize,
)> {
    let file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(writable)
        .open(image)
        .await
        .with_context(|| format!("Failed to open image: {}", image.display()))?;

    let block_dev = StreamBlockDevice(FromTokio::new(file));
    let stream = LargePageStream::new(block_dev, page_size);

    let fs = fatrs::FileSystem::new(stream, FsOptions::new())
        .await
        .context("Failed to mount FAT filesystem")?;

    Ok((fs, page_size))
}

pub async fn run(cli: Cli) -> Result<()> {
    let page_size = effective_page_size(cli.page_size);

    match cli.command {
        Command::Ls {
            image,
            path,
            long,
            recursive,
        } => cmd_ls(&image, &path, long, recursive, page_size).await,
        Command::Info { image } => cmd_info(&image, page_size).await,
        Command::Cat { image, path } => cmd_cat(&image, &path, page_size).await,
        Command::Cp {
            image,
            source,
            dest,
            recursive,
        } => cmd_cp(&image, &source, &dest, recursive, page_size).await,
        Command::Mkdir {
            image,
            path,
            parents,
        } => cmd_mkdir(&image, &path, parents, page_size).await,
        Command::Rm {
            image,
            path,
            recursive,
        } => cmd_rm(&image, &path, recursive, page_size).await,
        Command::Create {
            image,
            size,
            fat_type,
            label,
            from,
        } => {
            cmd_create(
                &image,
                &size,
                fat_type,
                label.as_deref(),
                from.as_deref(),
                page_size,
            )
            .await
        }
        Command::Extract { image, dest } => cmd_extract(&image, &dest, page_size).await,
    }
}

async fn cmd_ls(
    image: &Path,
    path: &str,
    long: bool,
    recursive: bool,
    page_size: usize,
) -> Result<()> {
    let (fs, _) = open_fs_buffered(image, false, page_size).await?;

    let root = fs.root_dir();
    let path = path.trim_start_matches('/');

    let dir = if path.is_empty() {
        root
    } else {
        root.open_dir(path)
            .await
            .with_context(|| format!("Failed to open directory: {}", path))?
    };

    list_directory(&dir, path, long, recursive, 0).await
}

async fn list_directory<IO: fatrs::ReadWriteSeek>(
    dir: &fatrs::Dir<'_, IO, fatrs::DefaultTimeProvider, fatrs::LossyOemCpConverter>,
    path: &str,
    long: bool,
    recursive: bool,
    depth: usize,
) -> Result<()>
where
    IO::Error: std::error::Error + Send + Sync + 'static,
{
    let mut iter = dir.iter();
    let mut entries = Vec::new();

    while let Some(entry_result) = iter.next().await {
        match entry_result {
            Ok(entry) => {
                let name = entry.file_name();
                if name.as_str() == "." || name.as_str() == ".." {
                    continue;
                }
                entries.push((
                    name.to_string(),
                    entry.is_dir(),
                    entry.len(),
                    entry.modified(),
                ));
            }
            Err(e) => {
                eprintln!("Error reading entry: {:?}", e);
            }
        }
    }

    // Sort: directories first, then by name
    entries.sort_by(|a, b| match (a.1, b.1) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => a.0.to_lowercase().cmp(&b.0.to_lowercase()),
    });

    if recursive && depth > 0 {
        println!("\n{}:", if path.is_empty() { "/" } else { path });
    }

    for (name, is_dir, size, modified) in &entries {
        if long {
            let type_char = if *is_dir { 'd' } else { '-' };
            let size_str = if *is_dir {
                "   <DIR>".to_string()
            } else {
                format_size(*size)
            };
            println!(
                "{}rw-r--r-- {:>8} {:04}-{:02}-{:02} {:02}:{:02} {}",
                type_char,
                size_str,
                modified.date.year,
                modified.date.month,
                modified.date.day,
                modified.time.hour,
                modified.time.min,
                name
            );
        } else {
            let suffix = if *is_dir { "/" } else { "" };
            print!("{}{}\t", name, suffix);
        }
    }

    if !long && !entries.is_empty() {
        println!();
    }

    if recursive {
        for (name, is_dir, _, _) in &entries {
            if *is_dir {
                let subpath = if path.is_empty() {
                    name.clone()
                } else {
                    format!("{}/{}", path, name)
                };
                let subdir = dir.open_dir(name).await?;
                Box::pin(list_directory(
                    &subdir,
                    &subpath,
                    long,
                    recursive,
                    depth + 1,
                ))
                .await?;
            }
        }
    }

    Ok(())
}

async fn cmd_info(image: &Path, page_size: usize) -> Result<()> {
    let metadata = tokio::fs::metadata(image).await?;

    let (fs, effective_page_size) = open_fs_buffered(image, false, page_size).await?;

    let stats = fs.stats().await?;

    println!("FAT Filesystem Information");
    println!("==========================");
    println!("Image file:      {}", image.display());
    println!("Image size:      {} bytes", metadata.len());
    println!("FAT type:        {:?}", fs.fat_type());
    println!(
        "Volume label:    {}",
        String::from_utf8_lossy(fs.volume_label_as_bytes()).trim()
    );
    println!("Volume ID:       {:08X}", fs.volume_id());
    println!("Cluster size:    {} bytes", stats.cluster_size());
    println!("Total clusters:  {}", stats.total_clusters());
    println!("Free clusters:   {}", stats.free_clusters());
    println!(
        "Total space:     {} bytes ({:.1} MB)",
        stats.total_clusters() as u64 * stats.cluster_size() as u64,
        (stats.total_clusters() as f64 * stats.cluster_size() as f64) / (1024.0 * 1024.0)
    );
    println!(
        "Free space:      {} bytes ({:.1} MB)",
        stats.free_clusters() as u64 * stats.cluster_size() as u64,
        (stats.free_clusters() as f64 * stats.cluster_size() as f64) / (1024.0 * 1024.0)
    );
    println!("I/O buffer:      {} bytes", effective_page_size);

    Ok(())
}

async fn cmd_cat(image: &Path, path: &str, page_size: usize) -> Result<()> {
    let (fs, _) = open_fs_buffered(image, false, page_size).await?;

    let root = fs.root_dir();
    let path = path.trim_start_matches('/');

    let mut fat_file = root
        .open_file(path)
        .await
        .with_context(|| format!("Failed to open file: {}", path))?;

    use embedded_io_async::Read;
    use std::io::Write;

    let mut buffer = [0u8; 8192];
    let mut stdout = std::io::stdout();

    loop {
        let n = fat_file.read(&mut buffer).await?;
        if n == 0 {
            break;
        }
        stdout.write_all(&buffer[..n])?;
    }

    Ok(())
}

async fn cmd_cp(
    image: &Path,
    source: &str,
    dest: &str,
    recursive: bool,
    page_size: usize,
) -> Result<()> {
    // Paths prefixed with : are inside the image
    let src_in_image = source.starts_with(':');
    let dst_in_image = dest.starts_with(':');

    let src_path = source.trim_start_matches(':');
    let dst_path = dest.trim_start_matches(':');

    if src_in_image && dst_in_image {
        anyhow::bail!("Cannot copy within the same image (both paths start with :)");
    }

    if !src_in_image && !dst_in_image {
        anyhow::bail!("At least one path must be inside the image (prefix with :)");
    }

    let (fs, _) = open_fs_buffered(image, dst_in_image, page_size).await?;

    let root = fs.root_dir();

    if src_in_image {
        // Copy from image to host filesystem
        copy_from_image(
            &root,
            src_path.trim_start_matches('/'),
            Path::new(dst_path),
            recursive,
        )
        .await
    } else {
        // Copy from host filesystem to image
        copy_to_image(
            &root,
            Path::new(src_path),
            dst_path.trim_start_matches('/'),
            recursive,
        )
        .await
    }
}

async fn copy_from_image<IO: fatrs::ReadWriteSeek>(
    root: &fatrs::Dir<'_, IO, fatrs::DefaultTimeProvider, fatrs::LossyOemCpConverter>,
    src_path: &str,
    dst_path: &Path,
    recursive: bool,
) -> Result<()>
where
    IO::Error: std::error::Error + Send + Sync + 'static,
{
    use embedded_io_async::Read;

    // Handle root directory specially - when src_path is empty, iterate over root directly
    let is_root_or_dir = if src_path.is_empty() {
        true // Root directory
    } else {
        root.open_dir(src_path).await.is_ok()
    };

    if is_root_or_dir {
        if !recursive && !src_path.is_empty() {
            anyhow::bail!("Source is a directory, use -r to copy recursively");
        }

        tokio::fs::create_dir_all(dst_path).await?;

        // Get directory iterator - for root use root directly, otherwise open the subdir
        let dir_to_iter;
        let iter_ref: &fatrs::Dir<'_, IO, _, _>;
        if src_path.is_empty() {
            iter_ref = root;
        } else {
            dir_to_iter = root.open_dir(src_path).await?;
            iter_ref = &dir_to_iter;
        }

        let mut iter = iter_ref.iter();
        while let Some(entry_result) = iter.next().await {
            if let Ok(entry) = entry_result {
                let name = entry.file_name();
                if name.as_str() == "." || name.as_str() == ".." {
                    continue;
                }

                let new_src = if src_path.is_empty() {
                    name.to_string()
                } else {
                    format!("{}/{}", src_path, name)
                };
                let new_dst = dst_path.join(name.as_str());

                Box::pin(copy_from_image(root, &new_src, &new_dst, recursive)).await?;
            }
        }
    } else {
        // It's a file
        let mut fat_file = root
            .open_file(src_path)
            .await
            .with_context(|| format!("Failed to open file: {}", src_path))?;

        let mut host_file = tokio::fs::File::create(dst_path)
            .await
            .with_context(|| format!("Failed to create file: {}", dst_path.display()))?;

        let mut buffer = [0u8; 8192];
        loop {
            let n = fat_file.read(&mut buffer).await?;
            if n == 0 {
                break;
            }
            tokio::io::AsyncWriteExt::write_all(&mut host_file, &buffer[..n]).await?;
        }

        println!("Copied: {} -> {}", src_path, dst_path.display());
    }

    Ok(())
}

async fn copy_to_image<IO: fatrs::ReadWriteSeek>(
    root: &fatrs::Dir<'_, IO, fatrs::DefaultTimeProvider, fatrs::LossyOemCpConverter>,
    src_path: &Path,
    dst_path: &str,
    recursive: bool,
) -> Result<()>
where
    IO::Error: std::error::Error + Send + Sync + 'static,
{
    use embedded_io_async::Write;

    let metadata = tokio::fs::metadata(src_path)
        .await
        .with_context(|| format!("Failed to stat: {}", src_path.display()))?;

    if metadata.is_dir() {
        if !recursive {
            anyhow::bail!("Source is a directory, use -r to copy recursively");
        }

        // Create directory in image
        if !dst_path.is_empty() {
            root.create_dir(dst_path).await.ok(); // Ignore if exists
        }

        let mut read_dir = tokio::fs::read_dir(src_path).await?;
        while let Some(entry) = read_dir.next_entry().await? {
            let name = entry.file_name();
            let name_str = name.to_string_lossy();

            let new_src = src_path.join(&name);
            let new_dst = if dst_path.is_empty() {
                name_str.to_string()
            } else {
                format!("{}/{}", dst_path, name_str)
            };

            Box::pin(copy_to_image(root, &new_src, &new_dst, recursive)).await?;
        }
    } else {
        // It's a file
        let mut host_file = tokio::fs::File::open(src_path)
            .await
            .with_context(|| format!("Failed to open file: {}", src_path.display()))?;

        let mut fat_file = root
            .create_file(dst_path)
            .await
            .with_context(|| format!("Failed to create file in image: {}", dst_path))?;

        let mut buffer = [0u8; 8192];
        loop {
            let n = tokio::io::AsyncReadExt::read(&mut host_file, &mut buffer).await?;
            if n == 0 {
                break;
            }
            fat_file.write_all(&buffer[..n]).await?;
        }
        fat_file.flush().await?;

        println!("Copied: {} -> :{}", src_path.display(), dst_path);
    }

    Ok(())
}

async fn cmd_mkdir(image: &Path, path: &str, parents: bool, page_size: usize) -> Result<()> {
    let (fs, _) = open_fs_buffered(image, true, page_size).await?;

    let root = fs.root_dir();
    let path = path.trim_start_matches('/');

    if parents {
        // Create parent directories as needed
        let parts: Vec<&str> = path.split('/').filter(|s| !s.is_empty()).collect();
        let mut current = String::new();

        for part in parts {
            if !current.is_empty() {
                current.push('/');
            }
            current.push_str(part);

            // Try to create, ignore if exists
            root.create_dir(&current).await.ok();
        }
    } else {
        root.create_dir(path)
            .await
            .with_context(|| format!("Failed to create directory: {}", path))?;
    }

    println!("Created directory: {}", path);
    Ok(())
}

async fn cmd_rm(image: &Path, path: &str, recursive: bool, page_size: usize) -> Result<()> {
    let (fs, _) = open_fs_buffered(image, true, page_size).await?;

    let root = fs.root_dir();
    let path = path.trim_start_matches('/');

    if recursive {
        remove_recursive(&root, path).await?;
    } else {
        root.remove(path)
            .await
            .with_context(|| format!("Failed to remove: {}", path))?;
    }

    println!("Removed: {}", path);
    Ok(())
}

async fn remove_recursive<IO: fatrs::ReadWriteSeek>(
    root: &fatrs::Dir<'_, IO, fatrs::DefaultTimeProvider, fatrs::LossyOemCpConverter>,
    path: &str,
) -> Result<()>
where
    IO::Error: std::error::Error + Send + Sync + 'static,
{
    // Check if it's a directory
    if let Ok(dir) = root.open_dir(path).await {
        // Remove contents first
        let mut iter = dir.iter();
        let mut entries = Vec::new();

        while let Some(entry_result) = iter.next().await {
            if let Ok(entry) = entry_result {
                let name = entry.file_name();
                if name.as_str() == "." || name.as_str() == ".." {
                    continue;
                }
                entries.push(name.to_string());
            }
        }
        drop(iter);
        drop(dir);

        for name in entries {
            let child_path = format!("{}/{}", path, name);
            Box::pin(remove_recursive(root, &child_path)).await?;
        }
    }

    // Remove the item itself
    root.remove(path).await?;
    Ok(())
}

async fn cmd_create(
    image: &Path,
    size: &str,
    fat_type: u8,
    label: Option<&str>,
    from: Option<&Path>,
    page_size: usize,
) -> Result<()> {
    let size_bytes = parse_size(size)?;

    // Create the file
    let file = tokio::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open(image)
        .await
        .with_context(|| format!("Failed to create image: {}", image.display()))?;

    // Set file size
    file.set_len(size_bytes).await?;

    let mut io = FromTokio::new(file);

    // Set up format options
    let mut options = FormatVolumeOptions::new();

    options = match fat_type {
        12 => options.fat_type(FatType::Fat12),
        16 => options.fat_type(FatType::Fat16),
        32 => options.fat_type(FatType::Fat32),
        _ => anyhow::bail!("Invalid FAT type: {}. Must be 12, 16, or 32", fat_type),
    };

    if let Some(label) = label {
        let mut label_bytes = [0x20u8; 11]; // Space-padded
        let label_upper = label.to_uppercase();
        let len = label_upper.len().min(11);
        label_bytes[..len].copy_from_slice(&label_upper.as_bytes()[..len]);
        options = options.volume_label(label_bytes);
    }

    // Format the volume
    fatrs::format_volume(&mut io, options)
        .await
        .context("Failed to format volume")?;

    println!(
        "Created FAT{} image: {} ({} bytes)",
        fat_type,
        image.display(),
        size_bytes
    );

    // If source directory specified, copy contents using buffered I/O
    if let Some(from_path) = from {
        println!("Copying contents from: {}", from_path.display());

        // Wrap in LargePageStream for efficient copying
        let block_dev = StreamBlockDevice(io);
        let stream = LargePageStream::new(block_dev, page_size);

        let fs = fatrs::FileSystem::new(stream, FsOptions::new())
            .await
            .context("Failed to mount newly created filesystem")?;

        let root = fs.root_dir();

        copy_dir_to_image(&root, from_path, "").await?;

        println!("Done copying files.");
    }

    Ok(())
}

async fn copy_dir_to_image<IO: fatrs::ReadWriteSeek>(
    root: &fatrs::Dir<'_, IO, fatrs::DefaultTimeProvider, fatrs::LossyOemCpConverter>,
    src_dir: &Path,
    dst_prefix: &str,
) -> Result<()>
where
    IO::Error: std::error::Error + Send + Sync + 'static,
{
    use embedded_io_async::Write;

    let mut read_dir = tokio::fs::read_dir(src_dir).await?;

    while let Some(entry) = read_dir.next_entry().await? {
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        let src_path = entry.path();

        let dst_path = if dst_prefix.is_empty() {
            name_str.to_string()
        } else {
            format!("{}/{}", dst_prefix, name_str)
        };

        let metadata = entry.metadata().await?;

        if metadata.is_dir() {
            root.create_dir(&dst_path).await.ok();
            Box::pin(copy_dir_to_image(root, &src_path, &dst_path)).await?;
        } else {
            let mut host_file = tokio::fs::File::open(&src_path).await?;
            let mut fat_file = root.create_file(&dst_path).await?;

            let mut buffer = [0u8; 8192];
            loop {
                let n = tokio::io::AsyncReadExt::read(&mut host_file, &mut buffer).await?;
                if n == 0 {
                    break;
                }
                fat_file.write_all(&buffer[..n]).await?;
            }
            fat_file.flush().await?;

            println!("  Added: {}", dst_path);
        }
    }

    Ok(())
}

async fn cmd_extract(image: &Path, dest: &Path, page_size: usize) -> Result<()> {
    let (fs, _) = open_fs_buffered(image, false, page_size).await?;

    let root = fs.root_dir();

    tokio::fs::create_dir_all(dest).await?;

    println!("Extracting to: {}", dest.display());
    copy_from_image(&root, "", dest, true).await?;
    println!("Done.");

    Ok(())
}
