//! FUSE adapter for fatrs
//!
//! This module bridges FUSE operations to the fatrs library,
//! enabling mounting FAT images with transaction-safe support.

use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fuser::{
    FileAttr, FileType, Filesystem, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, Request,
    TimeOrNow,
};

use fatrs::{FileSystem as FatFileSystem, OemCpConverter, ReadWriteSeek, TimeProvider};
use log::{debug, trace};

const TTL: Duration = Duration::from_secs(1);
const ROOT_INODE: u64 = 1;

/// FUSE adapter for fatrs
///
/// This struct implements the FUSE Filesystem trait and delegates
/// operations to the underlying fatrs implementation.
pub struct FuseAdapter<IO: ReadWriteSeek, TP: TimeProvider, OCC: OemCpConverter>
where
    IO::Error: 'static,
{
    fs: FatFileSystem<IO, TP, OCC>,
    /// Tokio runtime handle for executing async operations in sync FUSE context
    runtime: tokio::runtime::Handle,
    /// Inode counter (FUSE requires unique inodes)
    next_inode: Arc<Mutex<u64>>,
    /// Map from inode to filesystem path
    inode_to_path: Arc<Mutex<HashMap<u64, PathBuf>>>,
    /// Map from filesystem path to inode (for reverse lookup)
    path_to_inode: Arc<Mutex<HashMap<PathBuf, u64>>>,
}

impl<IO: ReadWriteSeek, TP: TimeProvider, OCC: OemCpConverter> FuseAdapter<IO, TP, OCC>
where
    IO::Error: 'static,
{
    /// Create a new FUSE adapter with a runtime handle
    pub fn new(fs: FatFileSystem<IO, TP, OCC>, runtime: tokio::runtime::Handle) -> Self {
        // Initialize inode mappings
        let mut inode_to_path = HashMap::new();
        let mut path_to_inode = HashMap::new();

        // Pre-allocate root directory as inode 1
        inode_to_path.insert(ROOT_INODE, PathBuf::from("/"));
        path_to_inode.insert(PathBuf::from("/"), ROOT_INODE);

        Self {
            fs,
            runtime,
            next_inode: Arc::new(Mutex::new(2)), // Start from 2, root is 1
            inode_to_path: Arc::new(Mutex::new(inode_to_path)),
            path_to_inode: Arc::new(Mutex::new(path_to_inode)),
        }
    }

    /// Allocate a new inode for a given path
    /// Returns existing inode if path is already mapped
    fn allocate_inode(&self, path: PathBuf) -> u64 {
        // Check if path already has an inode
        {
            let path_map = self.path_to_inode.lock().unwrap();
            if let Some(&inode) = path_map.get(&path) {
                return inode;
            }
        }

        // Allocate new inode
        let mut next = self.next_inode.lock().unwrap();
        let inode = *next;
        *next += 1;
        drop(next); // Release lock before acquiring others

        // Store bidirectional mapping
        self.inode_to_path
            .lock()
            .unwrap()
            .insert(inode, path.clone());
        self.path_to_inode
            .lock()
            .unwrap()
            .insert(path.clone(), inode);

        trace!("Allocated inode {} for path {:?}", inode, path);
        inode
    }

    /// Get the filesystem path for an inode
    fn get_path(&self, inode: u64) -> Option<PathBuf> {
        self.inode_to_path.lock().unwrap().get(&inode).cloned()
    }

    /// Get the inode for a filesystem path
    fn get_inode(&self, path: &PathBuf) -> Option<u64> {
        self.path_to_inode.lock().unwrap().get(path).copied()
    }

    /// Helper to execute async operations in sync FUSE context
    /// This blocks the current thread until the async operation completes
    fn block_on<F, T>(&self, future: F) -> T
    where
        F: std::future::Future<Output = T>,
    {
        self.runtime.block_on(future)
    }

    /// Convert FAT attributes to FUSE FileAttr
    fn fat_to_fuse_attr<'a>(&self, ino: u64, entry: &fatrs::DirEntry<'a, IO, TP, OCC>) -> FileAttr {
        let size = if entry.is_dir() {
            0
        } else {
            entry.len()
        };

        let kind = if entry.is_dir() {
            FileType::Directory
        } else {
            FileType::RegularFile
        };

        // Convert FAT DateTime to SystemTime using chrono
        let fat_to_systemtime = |dt: fatrs::DateTime| -> SystemTime {
            use chrono::{NaiveDate, Timelike};

            // Create a NaiveDateTime from FAT components
            let naive_date = NaiveDate::from_ymd_opt(
                dt.date.year as i32,
                dt.date.month as u32,
                dt.date.day as u32,
            );

            if let Some(date) = naive_date {
                let naive_datetime = date.and_hms_milli_opt(
                    dt.time.hour as u32,
                    dt.time.min as u32,
                    dt.time.sec as u32,
                    dt.time.millis as u32,
                );

                if let Some(datetime) = naive_datetime {
                    // Convert to UTC and then to SystemTime
                    let timestamp = datetime.and_utc().timestamp();
                    let nanos = datetime.nanosecond();

                    if timestamp >= 0 {
                        return UNIX_EPOCH
                            + Duration::from_secs(timestamp as u64)
                            + Duration::from_nanos(nanos as u64);
                    }
                }
            }

            // Fallback to Unix epoch if conversion fails
            UNIX_EPOCH
        };

        // Get modification time
        let mtime = fat_to_systemtime(entry.modified());

        // Get creation time (FAT supports this)
        let crtime = fat_to_systemtime(entry.created());

        // Get access time (FAT has limited support - date only)
        // accessed() returns a Date, not DateTime, so we need to convert it
        let accessed_date = entry.accessed();
        let midnight = fatrs::Time::new(0, 0, 0, 0);
        let accessed_datetime = fatrs::DateTime::new(accessed_date, midnight);
        let atime = fat_to_systemtime(accessed_datetime);

        FileAttr {
            ino,
            size,
            blocks: size.div_ceil(512), // Round up to 512-byte blocks
            atime,
            mtime,
            ctime: mtime, // FAT doesn't have true ctime, use mtime
            crtime,
            kind,
            perm: if entry.is_dir() { 0o755 } else { 0o644 },
            nlink: 1,
            uid: 1000, // Default user
            gid: 1000, // Default group
            rdev: 0,
            blksize: 512,
            flags: 0,
        }
    }
}

/// Implement FUSE Filesystem trait for Unix platforms
#[cfg(unix)]
impl<IO, TP, OCC> Filesystem for FuseAdapter<IO, TP, OCC>
where
    IO: fatrs::ReadWriteSeek + Send,
    IO::Error: std::error::Error + 'static,
    TP: fatrs::TimeProvider + Send,
    OCC: fatrs::OemCpConverter + Send,
{
    /// Look up a directory entry by name and get its attributes
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        debug!("lookup(parent={}, name={:?})", parent, name);

        // Get parent directory path
        let parent_path = match self.get_path(parent) {
            Some(path) => path,
            None => {
                debug!("lookup: parent inode {} not found", parent);
                reply.error(libc::ENOENT);
                return;
            }
        };

        // Convert OsStr to string
        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                debug!("lookup: invalid UTF-8 in name");
                reply.error(libc::EINVAL);
                return;
            }
        };

        // Construct full path
        let full_path = if parent_path.to_str() == Some("/") {
            PathBuf::from(format!("/{}", name_str))
        } else {
            parent_path.join(name_str)
        };

        // Look up the entry in FAT filesystem
        let result = self.block_on(async {
            let root = self.fs.root_dir();

            // For root directory children
            if parent == ROOT_INODE {
                root.open_meta(name_str).await
            } else {
                // For nested paths, we need to navigate from root
                let path_str = full_path.to_str().ok_or(fatrs::Error::InvalidInput)?;
                root.open_meta(path_str.trim_start_matches('/')).await
            }
        });

        match result {
            Ok(entry) => {
                // Allocate or get existing inode for this path
                let inode = self.allocate_inode(full_path);

                // Convert to FUSE attributes
                let attr = self.fat_to_fuse_attr(inode, &entry);

                debug!("lookup: found {} as inode {}", name_str, inode);
                reply.entry(&TTL, &attr, 0);
            }
            Err(e) => {
                debug!("lookup: entry not found: {:?}", e);
                reply.error(libc::ENOENT);
            }
        }
    }

    /// Get file attributes
    fn getattr(&mut self, _req: &Request, ino: u64, reply: ReplyAttr) {
        debug!("getattr(ino={})", ino);

        // Special handling for root directory
        if ino == ROOT_INODE {
            let attr = FileAttr {
                ino: ROOT_INODE,
                size: 0,
                blocks: 0,
                atime: UNIX_EPOCH,
                mtime: UNIX_EPOCH,
                ctime: UNIX_EPOCH,
                crtime: UNIX_EPOCH,
                kind: FileType::Directory,
                perm: 0o755,
                nlink: 2,
                uid: 1000,
                gid: 1000,
                rdev: 0,
                blksize: 512,
                flags: 0,
            };
            reply.attr(&TTL, &attr);
            return;
        }

        // Look up the path for this inode
        let path = match self.get_path(ino) {
            Some(p) => p,
            None => {
                debug!("getattr: inode {} not found in mapping", ino);
                reply.error(libc::ENOENT);
                return;
            }
        };

        // Get metadata from FAT filesystem
        let result = self.block_on(async {
            let root = self.fs.root_dir();
            let path_str = path.to_str().ok_or(fatrs::Error::InvalidInput)?;
            root.open_meta(path_str.trim_start_matches('/')).await
        });

        match result {
            Ok(entry) => {
                let attr = self.fat_to_fuse_attr(ino, &entry);
                debug!("getattr: inode {} -> {:?}", ino, path);
                reply.attr(&TTL, &attr);
            }
            Err(e) => {
                debug!("getattr: failed to get metadata for {:?}: {:?}", path, e);
                reply.error(libc::ENOENT);
            }
        }
    }

    /// Read directory contents
    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        debug!("readdir(ino={}, offset={})", ino, offset);

        // Get directory path
        let dir_path = match self.get_path(ino) {
            Some(p) => p,
            None => {
                debug!("readdir: inode {} not found", ino);
                reply.error(libc::ENOENT);
                return;
            }
        };

        // Read directory entries from FAT filesystem
        let result = self.block_on(async {
            let root = self.fs.root_dir();

            // Open the directory
            let dir = if ino == ROOT_INODE {
                root
            } else {
                let path_str = dir_path.to_str().ok_or(fatrs::Error::InvalidInput)?;
                root.open_dir(path_str.trim_start_matches('/')).await?
            };

            // Collect all directory entries
            let mut entries = Vec::new();
            let mut iter = dir.iter();
            while let Some(entry_result) = iter.next().await {
                match entry_result {
                    Ok(entry) => entries.push(entry),
                    Err(e) => return Err(e),
                }
            }

            Ok::<Vec<_>, fatrs::Error<IO::Error>>(entries)
        });

        match result {
            Ok(entries) => {
                let mut current_offset = offset;

                // Add . and .. entries (offset 0 and 1)
                // Note: reply.add takes the offset for the NEXT entry
                if current_offset == 0 {
                    if reply.add(ino, 1, FileType::Directory, ".") {
                        reply.ok();
                        return;
                    }
                    current_offset += 1;
                }

                if current_offset == 1 {
                    let parent_ino = ino; // TODO: track parent properly
                    if reply.add(parent_ino, 2, FileType::Directory, "..") {
                        reply.ok();
                        return;
                    }
                    current_offset += 1;
                }

                // Add actual directory entries (starting from offset 2)
                for (i, entry) in entries.iter().enumerate() {
                    let entry_offset = i as i64 + 2;

                    if entry_offset < current_offset {
                        continue;
                    }

                    // Construct full path for this entry
                    let entry_name = entry.file_name();
                    let entry_path = if dir_path.to_str() == Some("/") {
                        PathBuf::from(format!("/{}", entry_name))
                    } else {
                        dir_path.join(entry_name.as_str())
                    };

                    // Allocate inode for this entry
                    let entry_ino = self.allocate_inode(entry_path);

                    // Determine file type
                    let file_type = if entry.is_dir() {
                        FileType::Directory
                    } else {
                        FileType::RegularFile
                    };

                    // Add to reply buffer
                    // Note: next offset is entry_offset + 1
                    if reply.add(entry_ino, entry_offset + 1, file_type, entry_name.as_str()) {
                        // Buffer is full
                        reply.ok();
                        return;
                    }
                }

                debug!("readdir: returned {} entries", entries.len());
                reply.ok();
            }
            Err(e) => {
                debug!("readdir: failed to read directory {:?}: {:?}", dir_path, e);
                reply.error(libc::EIO);
            }
        }
    }

    /// Read data from a file
    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock: Option<u64>,
        reply: ReplyData,
    ) {
        debug!("read(ino={}, offset={}, size={})", ino, offset, size);

        // Get file path
        let file_path = match self.get_path(ino) {
            Some(p) => p,
            None => {
                debug!("read: inode {} not found", ino);
                reply.error(libc::ENOENT);
                return;
            }
        };

        // Read file data from FAT filesystem
        let result = self.block_on(async {
            use embedded_io_async::{Read as _, Seek as _};

            let root = self.fs.root_dir();

            // Open the file
            let path_str = file_path.to_str().ok_or(fatrs::Error::InvalidInput)?;
            let mut file = root.open_file(path_str.trim_start_matches('/')).await?;

            // Seek to the requested offset
            if offset > 0 {
                file.seek(embedded_io_async::SeekFrom::Start(offset as u64))
                    .await?;
            }

            // Read data
            let mut buffer = vec![0u8; size as usize];
            let bytes_read = file.read(&mut buffer).await?;

            // Truncate buffer to actual bytes read
            buffer.truncate(bytes_read);

            Ok::<Vec<u8>, fatrs::Error<IO::Error>>(buffer)
        });

        match result {
            Ok(data) => {
                debug!(
                    "read: successfully read {} bytes from {:?}",
                    data.len(),
                    file_path
                );
                reply.data(&data);
            }
            Err(e) => {
                debug!("read: failed to read file {:?}: {:?}", file_path, e);
                reply.error(libc::EIO);
            }
        }
    }

    /// Write data to a file
    fn write(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        data: &[u8],
        _write_flags: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: fuser::ReplyWrite,
    ) {
        debug!("write(ino={}, offset={}, size={})", ino, offset, data.len());

        // Get file path
        let file_path = match self.get_path(ino) {
            Some(p) => p,
            None => {
                debug!("write: inode {} not found", ino);
                reply.error(libc::ENOENT);
                return;
            }
        };

        // Write file data to FAT filesystem
        let result = self.block_on(async {
            use embedded_io_async::{Seek as _, Write as _};

            let root = self.fs.root_dir();

            // Open the file for writing
            let path_str = file_path.to_str().ok_or(fatrs::Error::InvalidInput)?;
            let mut file = root.open_file(path_str.trim_start_matches('/')).await?;

            // Seek to the requested offset
            if offset > 0 {
                file.seek(embedded_io_async::SeekFrom::Start(offset as u64))
                    .await?;
            }

            // Write data
            file.write_all(data).await?;

            // Flush to ensure data is written
            embedded_io_async::Write::flush(&mut file).await?;

            Ok::<usize, fatrs::Error<IO::Error>>(data.len())
        });

        match result {
            Ok(bytes_written) => {
                debug!(
                    "write: successfully wrote {} bytes to {:?}",
                    bytes_written, file_path
                );
                reply.written(bytes_written as u32);
            }
            Err(e) => {
                debug!("write: failed to write to file {:?}: {:?}", file_path, e);
                reply.error(libc::EIO);
            }
        }
    }

    /// Create and open a file
    fn create(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: fuser::ReplyCreate,
    ) {
        debug!("create(parent={}, name={:?})", parent, name);

        // Get parent directory path
        let parent_path = match self.get_path(parent) {
            Some(path) => path,
            None => {
                debug!("create: parent inode {} not found", parent);
                reply.error(libc::ENOENT);
                return;
            }
        };

        // Convert OsStr to string
        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                debug!("create: invalid UTF-8 in name");
                reply.error(libc::EINVAL);
                return;
            }
        };

        // Construct full path
        let full_path = if parent_path.to_str() == Some("/") {
            PathBuf::from(format!("/{}", name_str))
        } else {
            parent_path.join(name_str)
        };

        // Create the file in FAT filesystem
        let result = self.block_on(async {
            let root = self.fs.root_dir();

            // Create the file
            let path_str = full_path.to_str().ok_or(fatrs::Error::InvalidInput)?;
            let file = if parent == ROOT_INODE {
                root.create_file(name_str).await?
            } else {
                root.create_file(path_str.trim_start_matches('/')).await?
            };

            // Get the newly created file's metadata
            let entry = if parent == ROOT_INODE {
                root.open_meta(name_str).await?
            } else {
                root.open_meta(path_str.trim_start_matches('/')).await?
            };

            Ok::<_, fatrs::Error<IO::Error>>((file, entry))
        });

        match result {
            Ok((_file, entry)) => {
                // Allocate inode for the new file
                let inode = self.allocate_inode(full_path.clone());

                // Convert to FUSE attributes
                let attr = self.fat_to_fuse_attr(inode, &entry);

                debug!("create: created {:?} as inode {}", full_path, inode);

                // Reply with creation info
                // fh (file handle) = 0, flags = 0
                reply.created(&TTL, &attr, 0, 0, 0);
            }
            Err(e) => {
                debug!("create: failed to create file {:?}: {:?}", full_path, e);
                reply.error(libc::EIO);
            }
        }
    }

    /// Create a directory
    fn mkdir(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        debug!("mkdir(parent={}, name={:?})", parent, name);

        // Get parent directory path
        let parent_path = match self.get_path(parent) {
            Some(path) => path,
            None => {
                debug!("mkdir: parent inode {} not found", parent);
                reply.error(libc::ENOENT);
                return;
            }
        };

        // Convert OsStr to string
        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                debug!("mkdir: invalid UTF-8 in name");
                reply.error(libc::EINVAL);
                return;
            }
        };

        // Construct full path
        let full_path = if parent_path.to_str() == Some("/") {
            PathBuf::from(format!("/{}", name_str))
        } else {
            parent_path.join(name_str)
        };

        // Create the directory in FAT filesystem
        let result = self.block_on(async {
            let root = self.fs.root_dir();

            // Create the directory
            let path_str = full_path.to_str().ok_or(fatrs::Error::InvalidInput)?;
            if parent == ROOT_INODE {
                root.create_dir(name_str).await?;
            } else {
                root.create_dir(path_str.trim_start_matches('/')).await?;
            }

            // Get the newly created directory's metadata
            let entry = if parent == ROOT_INODE {
                root.open_meta(name_str).await?
            } else {
                root.open_meta(path_str.trim_start_matches('/')).await?
            };

            Ok::<_, fatrs::Error<IO::Error>>(entry)
        });

        match result {
            Ok(entry) => {
                // Allocate inode for the new directory
                let inode = self.allocate_inode(full_path.clone());

                // Convert to FUSE attributes
                let attr = self.fat_to_fuse_attr(inode, &entry);

                debug!("mkdir: created {:?} as inode {}", full_path, inode);
                reply.entry(&TTL, &attr, 0);
            }
            Err(e) => {
                debug!("mkdir: failed to create directory {:?}: {:?}", full_path, e);
                reply.error(libc::EIO);
            }
        }
    }

    /// Remove a file
    fn unlink(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        debug!("unlink(parent={}, name={:?})", parent, name);

        // Get parent directory path
        let parent_path = match self.get_path(parent) {
            Some(path) => path,
            None => {
                debug!("unlink: parent inode {} not found", parent);
                reply.error(libc::ENOENT);
                return;
            }
        };

        // Convert OsStr to string
        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                debug!("unlink: invalid UTF-8 in name");
                reply.error(libc::EINVAL);
                return;
            }
        };

        // Construct full path
        let full_path = if parent_path.to_str() == Some("/") {
            PathBuf::from(format!("/{}", name_str))
        } else {
            parent_path.join(name_str)
        };

        // Remove the file from FAT filesystem
        let result = self.block_on(async {
            let root = self.fs.root_dir();

            // Remove the file
            let path_str = full_path.to_str().ok_or(fatrs::Error::InvalidInput)?;
            if parent == ROOT_INODE {
                root.remove(name_str).await
            } else {
                root.remove(path_str.trim_start_matches('/')).await
            }
        });

        match result {
            Ok(()) => {
                debug!("unlink: removed {:?}", full_path);

                // Remove from inode mappings
                if let Some(inode) = self.get_inode(&full_path) {
                    self.inode_to_path.lock().unwrap().remove(&inode);
                    self.path_to_inode.lock().unwrap().remove(&full_path);
                }

                reply.ok();
            }
            Err(e) => {
                debug!("unlink: failed to remove file {:?}: {:?}", full_path, e);
                reply.error(libc::EIO);
            }
        }
    }

    /// Remove a directory
    fn rmdir(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: fuser::ReplyEmpty) {
        debug!("rmdir(parent={}, name={:?})", parent, name);

        // Get parent directory path
        let parent_path = match self.get_path(parent) {
            Some(path) => path,
            None => {
                debug!("rmdir: parent inode {} not found", parent);
                reply.error(libc::ENOENT);
                return;
            }
        };

        // Convert OsStr to string
        let name_str = match name.to_str() {
            Some(s) => s,
            None => {
                debug!("rmdir: invalid UTF-8 in name");
                reply.error(libc::EINVAL);
                return;
            }
        };

        // Construct full path
        let full_path = if parent_path.to_str() == Some("/") {
            PathBuf::from(format!("/{}", name_str))
        } else {
            parent_path.join(name_str)
        };

        // Remove the directory from FAT filesystem
        let result = self.block_on(async {
            let root = self.fs.root_dir();

            // Remove the directory
            let path_str = full_path.to_str().ok_or(fatrs::Error::InvalidInput)?;
            if parent == ROOT_INODE {
                root.remove(name_str).await
            } else {
                root.remove(path_str.trim_start_matches('/')).await
            }
        });

        match result {
            Ok(()) => {
                debug!("rmdir: removed {:?}", full_path);

                // Remove from inode mappings
                if let Some(inode) = self.get_inode(&full_path) {
                    self.inode_to_path.lock().unwrap().remove(&inode);
                    self.path_to_inode.lock().unwrap().remove(&full_path);
                }

                reply.ok();
            }
            Err(e) => {
                debug!("rmdir: failed to remove directory {:?}: {:?}", full_path, e);
                reply.error(libc::EIO);
            }
        }
    }

    /// Rename a file or directory
    fn rename(
        &mut self,
        _req: &Request,
        parent: u64,
        name: &OsStr,
        newparent: u64,
        newname: &OsStr,
        _flags: u32,
        reply: fuser::ReplyEmpty,
    ) {
        debug!(
            "rename(parent={}, name={:?}, newparent={}, newname={:?})",
            parent, name, newparent, newname
        );

        // Get old parent path
        let old_parent_path = match self.get_path(parent) {
            Some(path) => path,
            None => {
                debug!("rename: old parent inode {} not found", parent);
                reply.error(libc::ENOENT);
                return;
            }
        };

        // Get new parent path
        let new_parent_path = match self.get_path(newparent) {
            Some(path) => path,
            None => {
                debug!("rename: new parent inode {} not found", newparent);
                reply.error(libc::ENOENT);
                return;
            }
        };

        // Convert names to strings
        let old_name_str = match name.to_str() {
            Some(s) => s,
            None => {
                debug!("rename: invalid UTF-8 in old name");
                reply.error(libc::EINVAL);
                return;
            }
        };

        let new_name_str = match newname.to_str() {
            Some(s) => s,
            None => {
                debug!("rename: invalid UTF-8 in new name");
                reply.error(libc::EINVAL);
                return;
            }
        };

        // Construct full paths
        let old_path = if old_parent_path.to_str() == Some("/") {
            PathBuf::from(format!("/{}", old_name_str))
        } else {
            old_parent_path.join(old_name_str)
        };

        let new_path = if new_parent_path.to_str() == Some("/") {
            PathBuf::from(format!("/{}", new_name_str))
        } else {
            new_parent_path.join(new_name_str)
        };

        // Rename in FAT filesystem
        let result = self.block_on(async {
            let root = self.fs.root_dir();

            let old_path_str = old_path.to_str().ok_or(fatrs::Error::InvalidInput)?;
            let new_path_str = new_path.to_str().ok_or(fatrs::Error::InvalidInput)?;

            root.rename(
                old_path_str.trim_start_matches('/'),
                &root,
                new_path_str.trim_start_matches('/'),
            )
            .await
        });

        match result {
            Ok(()) => {
                debug!("rename: renamed {:?} to {:?}", old_path, new_path);

                // Update inode mappings
                if let Some(inode) = self.get_inode(&old_path) {
                    self.path_to_inode.lock().unwrap().remove(&old_path);
                    self.inode_to_path
                        .lock()
                        .unwrap()
                        .insert(inode, new_path.clone());
                    self.path_to_inode.lock().unwrap().insert(new_path, inode);
                }

                reply.ok();
            }
            Err(e) => {
                debug!(
                    "rename: failed to rename {:?} to {:?}: {:?}",
                    old_path, new_path, e
                );
                reply.error(libc::EIO);
            }
        }
    }

    /// Set file attributes (size, timestamps, etc.)
    fn setattr(
        &mut self,
        _req: &Request,
        ino: u64,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        size: Option<u64>,
        _atime: Option<TimeOrNow>,
        _mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        debug!("setattr(ino={}, size={:?})", ino, size);

        // Get file path
        let file_path = match self.get_path(ino) {
            Some(p) => p,
            None => {
                debug!("setattr: inode {} not found", ino);
                reply.error(libc::ENOENT);
                return;
            }
        };

        // Handle truncate (size change)
        if let Some(new_size) = size {
            let result = self.block_on(async {
                use embedded_io_async::Seek as _;

                let root = self.fs.root_dir();

                // Open the file
                let path_str = file_path.to_str().ok_or(fatrs::Error::InvalidInput)?;
                let mut file = root.open_file(path_str.trim_start_matches('/')).await?;

                // Seek to the new size position
                file.seek(embedded_io_async::SeekFrom::Start(new_size))
                    .await?;

                // Truncate at this position
                file.truncate().await?;

                // Flush changes
                embedded_io_async::Write::flush(&mut file).await?;

                // Get updated metadata
                root.open_meta(path_str.trim_start_matches('/')).await
            });

            match result {
                Ok(entry) => {
                    let attr = self.fat_to_fuse_attr(ino, &entry);
                    debug!("setattr: truncated {:?} to {} bytes", file_path, new_size);
                    reply.attr(&TTL, &attr);
                }
                Err(e) => {
                    debug!("setattr: failed to truncate {:?}: {:?}", file_path, e);
                    reply.error(libc::EIO);
                }
            }
        } else {
            // If only changing attributes we don't support (mode, uid, gid, times), just return current attrs
            let result = self.block_on(async {
                let root = self.fs.root_dir();
                let path_str = file_path.to_str().ok_or(fatrs::Error::InvalidInput)?;
                root.open_meta(path_str.trim_start_matches('/')).await
            });

            match result {
                Ok(entry) => {
                    let attr = self.fat_to_fuse_attr(ino, &entry);
                    reply.attr(&TTL, &attr);
                }
                Err(e) => {
                    debug!(
                        "setattr: failed to get metadata for {:?}: {:?}",
                        file_path, e
                    );
                    reply.error(libc::ENOENT);
                }
            }
        }
    }
}
