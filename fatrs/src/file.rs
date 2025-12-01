use core::cmp;

use crate::dir_entry::DirEntryEditor;
use crate::error::Error;
use crate::fs::{FileSystem, ReadWriteSeek};
use crate::io::{IoBase, Read, Seek, SeekFrom, Write};
use crate::time::{Date, DateTime, TimeProvider};

const MAX_FILE_SIZE: u32 = u32::MAX;

/// A FAT filesystem file object used for reading and writing data.
///
/// This struct is created by the `open_file` or `create_file` methods on `Dir`.
pub struct File<'a, IO: ReadWriteSeek, TP, OCC>
where
    IO::Error: 'static,
{
    context: FileContext,
    // file-system reference
    fs: &'a FileSystem<IO, TP, OCC>,
    // Lock type held by this file (if file-locking feature is enabled)
    #[cfg(feature = "file-locking")]
    lock_info: Option<crate::file_locking::LockType>,
}

/// A context of an existing [`File`].
///
/// This is obtained by calling [`File::close`] and can be used to resume
/// operations on a [`File`] with the [`DirEntry::to_file_with_context`](crate::dir_entry::DirEntry::to_file_with_context)
/// method. This can be useful for large files, because to `Seek` to the
/// end of the file would mean scanning the whole cluster chain which
/// has `O(n)` time complexity.
///
/// Phase 2 Optimizations:
/// - Contiguous file tracking for multi-cluster I/O
/// - Cluster chain checkpoints for logarithmic seek
#[derive(Clone)]
pub struct FileContext {
    // Note first_cluster is None if file is empty
    pub(crate) first_cluster: Option<u32>,
    // Note: if offset points between clusters current_cluster is the previous cluster
    pub(crate) current_cluster: Option<u32>,
    // current position in this file
    pub(crate) offset: u32,
    // file dir entry editor - None for root dir
    pub(crate) entry: Option<DirEntryEditor>,

    // Phase 2 Optimization: Contiguous file tracking
    // When true, file clusters are allocated sequentially and FAT traversal can be skipped
    #[cfg(feature = "multi-cluster-io")]
    pub(crate) is_contiguous: bool,

    // Phase 2 Optimization: Cluster chain checkpoints for O(log n) seeking
    // Stores (offset, cluster) pairs at regular intervals
    #[cfg(feature = "cluster-checkpoints")]
    pub(crate) checkpoints: [(u32, u32); 8], // Up to 8 checkpoints
    #[cfg(feature = "cluster-checkpoints")]
    pub(crate) checkpoint_count: u8,
}

/// An extent containing a file's data on disk.
///
/// This is created by the `extents` method on `File`, and represents
/// a byte range on the disk that contains a file's data. All values
/// are in bytes.
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[derive(Clone, Debug)]
pub struct Extent {
    pub offset: u64,
    pub size: u32,
}

impl<'a, IO: ReadWriteSeek, TP, OCC> File<'a, IO, TP, OCC> {
    pub(crate) fn new(
        first_cluster: Option<u32>,
        entry: Option<DirEntryEditor>,
        fs: &'a FileSystem<IO, TP, OCC>,
    ) -> Self {
        File {
            context: FileContext {
                first_cluster,
                entry,
                current_cluster: None, // cluster before first one
                offset: 0,
                #[cfg(feature = "multi-cluster-io")]
                is_contiguous: false, // Will be detected during allocation
                #[cfg(feature = "cluster-checkpoints")]
                checkpoints: [(0, 0); 8],
                #[cfg(feature = "cluster-checkpoints")]
                checkpoint_count: 0,
            },
            fs,
            #[cfg(feature = "file-locking")]
            lock_info: None,
        }
    }

    /// Create a new file with a lock held.
    /// This is used internally by locked file open operations.
    #[cfg(feature = "file-locking")]
    pub(crate) fn new_with_lock(
        first_cluster: Option<u32>,
        entry: Option<DirEntryEditor>,
        fs: &'a FileSystem<IO, TP, OCC>,
        lock_type: crate::file_locking::LockType,
    ) -> Self {
        File {
            context: FileContext {
                first_cluster,
                entry,
                current_cluster: None,
                offset: 0,
                #[cfg(feature = "multi-cluster-io")]
                is_contiguous: false,
                #[cfg(feature = "cluster-checkpoints")]
                checkpoints: [(0, 0); 8],
                #[cfg(feature = "cluster-checkpoints")]
                checkpoint_count: 0,
            },
            fs,
            lock_info: Some(lock_type),
        }
    }

    /// Create a file from a prexisting [`FileContext`] & [`FileSystem`].
    ///
    /// **WARNING** This method has the power to corrupt the filesystem when misused.
    /// Read and write access is allowed simultaneously, however two or more write accesses will corrupt the file system.
    /// Avoid concurrent write access to ensure file system stability.
    ///
    ///
    /// Prefer using [`DirEntry::try_to_file_with_context`](crate::dir_entry::DirEntry::try_to_file_with_context) where possible because
    /// it does some basic checks to avoid file corruption.
    pub(crate) fn new_from_context(context: FileContext, fs: &'a FileSystem<IO, TP, OCC>) -> Self {
        File {
            context,
            fs,
            #[cfg(feature = "file-locking")]
            lock_info: None,
        }
    }

    /// Truncate file in current position.
    ///
    /// # Errors
    ///
    /// `Error::Io` will be returned if the underlying storage object returned an I/O error.
    ///
    /// # Panics
    ///
    /// Will panic if this is the root directory.
    pub async fn truncate(&mut self) -> Result<(), Error<IO::Error>> {
        trace!("File::truncate");
        if let Some(ref mut e) = self.context.entry {
            e.set_size(self.context.offset);
            if self.context.offset == 0 {
                e.set_first_cluster(None, self.fs.fat_type());
            }
        } else {
            // Note: we cannot handle this case because there is no size field
            panic!("Trying to truncate a file without an entry");
        }
        if let Some(current_cluster) = self.context.current_cluster {
            // current cluster is none only if offset is 0
            debug_assert!(self.context.offset > 0);
            self.fs.truncate_cluster_chain(current_cluster).await
        } else {
            debug_assert!(self.context.offset == 0);
            if let Some(n) = self.context.first_cluster {
                self.fs.free_cluster_chain(n).await?;
                self.context.first_cluster = None;
            }
            Ok(())
        }
    }

    /// Phase 3 Optimization: Find the closest checkpoint to the target cluster index
    /// Returns (starting_cluster, clusters_already_traversed)
    #[cfg(feature = "cluster-checkpoints")]
    fn find_closest_checkpoint(&self, target_cluster_index: u32) -> (u32, u32) {
        let first_cluster = self.context.first_cluster.unwrap();

        // If no checkpoints recorded yet, start from beginning
        if self.context.checkpoint_count == 0 {
            return (first_cluster, 0);
        }

        // Find the checkpoint closest to but not exceeding target
        let mut best_cluster = first_cluster;
        let mut best_index = 0u32;

        for i in 0..self.context.checkpoint_count as usize {
            let (checkpoint_index, checkpoint_cluster) = self.context.checkpoints[i];
            // Use checkpoint if it's closer to target than our current best
            if checkpoint_index <= target_cluster_index && checkpoint_index > best_index {
                best_index = checkpoint_index;
                best_cluster = checkpoint_cluster;
            }
        }

        trace!(
            "Checkpoint seek: target={}, using checkpoint at index={} (saved {} cluster reads)",
            target_cluster_index,
            best_index,
            best_index
        );

        (best_cluster, best_index)
    }

    /// Phase 3 Optimization: Record a checkpoint at the current position
    /// Checkpoints are stored at exponentially increasing intervals for logarithmic seek
    #[cfg(feature = "cluster-checkpoints")]
    fn record_checkpoint(&mut self, cluster_index: u32, cluster: u32) {
        // Record checkpoints at intervals: 8, 16, 32, 64, 128, 256, 512, 1024 clusters
        // This gives O(log n) seek performance
        let checkpoint_interval = 1u32 << (self.context.checkpoint_count.min(7) as u32 + 3);

        if cluster_index > 0 && cluster_index % checkpoint_interval == 0 {
            let idx = self.context.checkpoint_count as usize;
            if idx < 8 {
                self.context.checkpoints[idx] = (cluster_index, cluster);
                self.context.checkpoint_count += 1;
                trace!("Recorded checkpoint: index={}, cluster={}", cluster_index, cluster);
            }
        }
    }

    // /// Get the extents of a file on disk.
    // ///
    // /// This returns an iterator over the byte ranges on-disk occupied by
    // /// this file.
    // pub fn extents(&mut self) -> impl Iterator<Item = Result<Extent, Error<IO::Error>>> + 'a {
    // let fs = self.fs;
    // let cluster_size = fs.cluster_size();
    // let mut bytes_left = match self.size() {
    //     Some(s) => s,
    //     None => return None.into_iter().flatten(),
    // };
    // let first = match self.context.first_cluster {
    //     Some(f) => f,
    //     None => return None.into_iter().flatten(),
    // };

    // Some(
    //     core::iter::once(Ok(first))
    //         .chain(fs.cluster_iter(first))
    //         .map(move |cluster_err| match cluster_err {
    //             Ok(cluster) => {
    //                 let size = cluster_size.min(bytes_left);
    //                 bytes_left -= size;
    //                 Ok(Extent {
    //                     offset: fs.offset_from_cluster(cluster),
    //                     size,
    //                 })
    //             }
    //             Err(e) => Err(e),
    //         }),
    // )
    // .into_iter()
    // .flatten()
    // todo!("extents needs to be implemented using AsyncIterator");
    // }

    pub(crate) fn abs_pos(&self) -> Option<u64> {
        // Returns current position relative to filesystem start
        // Note: when between clusters it returns position after previous cluster
        match self.context.current_cluster {
            Some(n) => {
                let cluster_size = self.fs.cluster_size();
                let offset_mod_cluster_size = self.context.offset % cluster_size;
                let offset_in_cluster = if offset_mod_cluster_size == 0 {
                    // position points between clusters - we are returning previous cluster so
                    // offset must be set to the cluster size
                    cluster_size
                } else {
                    offset_mod_cluster_size
                };
                let offset_in_fs = self.fs.offset_from_cluster(n) + u64::from(offset_in_cluster);
                Some(offset_in_fs)
            }
            None => None,
        }
    }

    async fn flush_dir_entry(&mut self) -> Result<(), Error<IO::Error>> {
        if let Some(ref mut e) = self.context.entry {
            e.flush(self.fs).await?;
        }
        Ok(())
    }

    /// Sets date and time of creation for this file.
    ///
    /// Note: it is set to a value from the `TimeProvider` when creating a file.
    /// Deprecated: if needed implement a custom `TimeProvider`.
    #[deprecated]
    pub fn set_created(&mut self, date_time: DateTime) {
        if let Some(ref mut e) = self.context.entry {
            e.set_created(date_time);
        }
    }

    /// Sets date of last access for this file.
    ///
    /// Note: it is overwritten by a value from the `TimeProvider` on every file read operation.
    /// Deprecated: if needed implement a custom `TimeProvider`.
    #[deprecated]
    pub fn set_accessed(&mut self, date: Date) {
        if let Some(ref mut e) = self.context.entry {
            e.set_accessed(date);
        }
    }

    /// Sets date and time of last modification for this file.
    ///
    /// Note: it is overwritten by a value from the `TimeProvider` on every file write operation.
    /// Deprecated: if needed implement a custom `TimeProvider`.
    #[deprecated]
    pub fn set_modified(&mut self, date_time: DateTime) {
        if let Some(ref mut e) = self.context.entry {
            e.set_modified(date_time);
        }
    }

    fn size(&self) -> Option<u32> {
        match self.context.entry {
            Some(ref e) => e.inner().size(),
            None => None,
        }
    }

    fn is_dir(&self) -> bool {
        match self.context.entry {
            Some(ref e) => e.inner().is_dir(),
            None => false,
        }
    }

    fn bytes_left_in_file(&self) -> Option<usize> {
        // Note: seeking beyond end of file is not allowed so overflow is impossible
        self.size().map(|s| (s - self.context.offset) as usize)
    }

    fn set_first_cluster(&mut self, cluster: u32) {
        self.context.first_cluster = Some(cluster);
        if let Some(ref mut e) = self.context.entry {
            e.set_first_cluster(self.context.first_cluster, self.fs.fat_type());
        }
    }

    pub(crate) fn first_cluster(&self) -> Option<u32> {
        self.context.first_cluster
    }

    #[allow(clippy::await_holding_refcell_ref)]
    async fn flush(&mut self) -> Result<(), Error<IO::Error>> {
        self.flush_dir_entry().await?;
        {
            let mut disk = self.fs.disk.lock().await;
            disk.flush().await?;
        }
        Ok(())
    }
}

impl<IO: ReadWriteSeek, TP: TimeProvider, OCC> File<'_, IO, TP, OCC> {
    fn update_dir_entry_after_write(&mut self) {
        let offset = self.context.offset;
        if let Some(ref mut e) = self.context.entry {
            let now = self.fs.options.time_provider.get_current_date_time();
            e.set_modified(now);
            if e.inner().size().is_some_and(|s| offset > s) {
                e.set_size(offset);
            }
        }
    }

    /// Manually close the file
    ///
    /// A [`FileContext`] is returned, which can be used in conjunction with the
    /// `to_file_with_context` API.
    ///
    /// **Note:** If this file was opened with a lock (using `open_file_locked` or
    /// `create_file_locked`), use [`close_and_unlock`](Self::close_and_unlock) instead
    /// to properly release the lock.
    #[allow(clippy::missing_errors_doc)]
    pub fn close(self) -> Result<FileContext, Error<IO::Error>> {
        #[cfg(feature = "file-locking")]
        if self.lock_info.is_some() {
            warn!("Closing locked file without calling close_and_unlock - lock will not be released");
        }

        Ok(FileContext {
            first_cluster: self.context.first_cluster,
            current_cluster: self.context.current_cluster,
            offset: self.context.offset,
            entry: self.context.entry.clone(),
            #[cfg(feature = "multi-cluster-io")]
            is_contiguous: self.context.is_contiguous,
            #[cfg(feature = "cluster-checkpoints")]
            checkpoints: self.context.checkpoints,
            #[cfg(feature = "cluster-checkpoints")]
            checkpoint_count: self.context.checkpoint_count,
        })
    }

    /// Close the file and release any held lock.
    ///
    /// This should be used for files opened with `open_file_locked` or `create_file_locked`.
    /// A [`FileContext`] is returned, which can be used to reopen the file.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// let file = dir.open_file_locked("data.txt").await?;
    /// // ... use file ...
    /// let context = file.close_and_unlock().await?;
    /// ```
    #[cfg(feature = "file-locking")]
    pub async fn close_and_unlock(self) -> Result<FileContext, Error<IO::Error>> {
        // Release the lock if one was held
        if let (Some(lock_type), Some(first_cluster)) = (self.lock_info, self.context.first_cluster) {
            let mut locks = self.fs.file_locks.lock().await;
            locks.unlock(first_cluster, lock_type);
        }

        Ok(FileContext {
            first_cluster: self.context.first_cluster,
            current_cluster: self.context.current_cluster,
            offset: self.context.offset,
            entry: self.context.entry.clone(),
            #[cfg(feature = "multi-cluster-io")]
            is_contiguous: self.context.is_contiguous,
            #[cfg(feature = "cluster-checkpoints")]
            checkpoints: self.context.checkpoints,
            #[cfg(feature = "cluster-checkpoints")]
            checkpoint_count: self.context.checkpoint_count,
        })
    }

    /// Check if this file holds a lock.
    #[cfg(feature = "file-locking")]
    pub fn is_locked(&self) -> bool {
        self.lock_info.is_some()
    }

    /// Get the lock type held by this file.
    #[cfg(feature = "file-locking")]
    pub fn lock_type(&self) -> Option<crate::file_locking::LockType> {
        self.lock_info
    }
}

impl<IO: ReadWriteSeek, TP, OCC> Drop for File<'_, IO, TP, OCC> {
    fn drop(&mut self) {
        if let Some(e) = &self.context.entry {
            if e.dirty() {
                warn!("Dropping dirty file before flushing");
                #[cfg(feature = "dirty-file-panic")]
                {
                    panic!("Dropping unflushed file");
                }
            }
        }
    }
}

// Note: derive cannot be used because of invalid bounds. See: https://github.com/rust-lang/rust/issues/26925
// Note: Cloning a file does NOT clone the lock - the cloned file is unlocked.
// This is intentional to prevent lock reference counting issues.
impl<IO: ReadWriteSeek, TP, OCC> Clone for File<'_, IO, TP, OCC> {
    fn clone(&self) -> Self {
        File {
            context: self.context.clone(),
            fs: self.fs,
            #[cfg(feature = "file-locking")]
            lock_info: None, // Clones don't inherit locks
        }
    }
}

impl<IO: ReadWriteSeek, TP, OCC> IoBase for File<'_, IO, TP, OCC>
where
    IO::Error: 'static,
{
    type Error = Error<IO::Error>;
}

impl<IO: ReadWriteSeek, TP: TimeProvider, OCC> Read for File<'_, IO, TP, OCC> {
    #[allow(clippy::too_many_lines)]
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        trace!("File::read");
        let cluster_size = self.fs.cluster_size();
        let current_cluster_opt = if self.context.offset % cluster_size == 0 {
            // next cluster
            match self.context.current_cluster {
                None => self.context.first_cluster,
                Some(n) => {
                    let r = self.fs.cluster_iter(n).next().await;
                    match r {
                        Some(Err(err)) => return Err(err),
                        Some(Ok(n)) => Some(n),
                        None => None,
                    }
                }
            }
        } else {
            self.context.current_cluster
        };
        let Some(current_cluster) = current_cluster_opt else {
            return Ok(0);
        };
        let offset_in_cluster = self.context.offset % cluster_size;
        let bytes_left_in_file = self.bytes_left_in_file().unwrap_or(buf.len());

        // Phase 2 Optimization: Multi-cluster I/O
        // If reading more than one cluster and multi-cluster-io is enabled, try batched read
        #[cfg(feature = "multi-cluster-io")]
        {
            if buf.len() > (cluster_size - offset_in_cluster) as usize {
                // Potential multi-cluster read
                trace!("attempting multi-cluster read");
                match crate::multi_cluster_io::read_contiguous(
                    self.fs,
                    current_cluster,
                    offset_in_cluster,
                    buf,
                )
                .await
                {
                    Ok(read_bytes) if read_bytes > 0 => {
                        // Multi-cluster read succeeded!
                        let read_bytes = cmp::min(read_bytes, bytes_left_in_file);

                        let old_offset = self.context.offset;
                        self.context.offset += read_bytes as u32;
                        let new_offset = self.context.offset;

                        // Update current cluster to match new offset
                        // FAT convention: when at a cluster boundary, current_cluster points to
                        // the previous cluster (the one just finished), not the next cluster.
                        let old_cluster_index = if old_offset == 0 {
                            0u32
                        } else {
                            (old_offset / cluster_size).saturating_sub(1)
                        };

                        let new_cluster_index = if new_offset > 0 && new_offset % cluster_size == 0 {
                            (new_offset / cluster_size).saturating_sub(1)
                        } else {
                            new_offset / cluster_size
                        };

                        let cluster_delta = new_cluster_index.saturating_sub(old_cluster_index);

                        if cluster_delta > 0 {
                            let mut cluster = current_cluster;
                            for _i in 0..cluster_delta {
                                let mut iter = self.fs.cluster_iter(cluster);
                                if let Some(Ok(next)) = iter.next().await {
                                    cluster = next;
                                    // Record checkpoint during sequential traversal
                                    #[cfg(feature = "cluster-checkpoints")]
                                    {
                                        let cluster_idx = old_cluster_index + _i + 1;
                                        self.record_checkpoint(cluster_idx, cluster);
                                    }
                                } else {
                                    break;
                                }
                            }
                            self.context.current_cluster = Some(cluster);
                        } else {
                            self.context.current_cluster = Some(current_cluster);
                        }

                        if let Some(ref mut e) = self.context.entry {
                            if self.fs.options.update_accessed_date {
                                let now = self.fs.options.time_provider.get_current_date();
                                e.set_accessed(now);
                            }
                        }
                        trace!("multi-cluster read: {} bytes", read_bytes);
                        return Ok(read_bytes);
                    }
                    _ => {
                        // Fall through to single-cluster read
                        trace!("falling back to single-cluster read");
                    }
                }
            }
        }

        // Original single-cluster read path
        let bytes_left_in_cluster = (cluster_size - offset_in_cluster) as usize;
        let read_size = cmp::min(cmp::min(buf.len(), bytes_left_in_cluster), bytes_left_in_file);
        if read_size == 0 {
            return Ok(0);
        }
        trace!("read {} bytes in cluster {}", read_size, current_cluster);
        let offset_in_fs = self.fs.offset_from_cluster(current_cluster) + u64::from(offset_in_cluster);
        #[allow(clippy::await_holding_refcell_ref)]
        let read_bytes = {
            let mut disk = self.fs.disk.lock().await;
            disk.seek(SeekFrom::Start(offset_in_fs)).await?;
            disk.read(&mut buf[..read_size]).await?
        };
        if read_bytes == 0 {
            return Ok(0);
        }
        self.context.offset += read_bytes as u32;
        self.context.current_cluster = Some(current_cluster);

        // Record checkpoint for sequential reads
        #[cfg(feature = "cluster-checkpoints")]
        if self.context.offset > 0 {
            let cluster_idx = (self.context.offset / cluster_size).saturating_sub(1);
            self.record_checkpoint(cluster_idx, current_cluster);
        }

        if let Some(ref mut e) = self.context.entry {
            if self.fs.options.update_accessed_date {
                let now = self.fs.options.time_provider.get_current_date();
                e.set_accessed(now);
            }
        }
        Ok(read_bytes)
    }
}

impl<IO: ReadWriteSeek, TP: TimeProvider, OCC> Write for File<'_, IO, TP, OCC> {
    #[allow(clippy::too_many_lines)]
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        trace!("File::write");
        let cluster_size = self.fs.cluster_size();
        let offset_in_cluster = self.context.offset % cluster_size;
        let bytes_left_until_max_file_size = (MAX_FILE_SIZE - self.context.offset) as usize;

        // Exit early if we are going to write no data
        if buf.is_empty() || bytes_left_until_max_file_size == 0 {
            return Ok(0);
        }

        // Mark the volume 'dirty'
        self.fs.set_dirty_flag(true).await?;

        // Phase 2 Optimization: Multi-cluster write for already allocated clusters
        // This provides the flash wear reduction benefit for large sequential writes
        #[cfg(feature = "multi-cluster-io")]
        {
            // Check if we're at a cluster boundary and writing more than one cluster
            if offset_in_cluster == 0 && buf.len() >= cluster_size as usize {
                // Get the cluster to write to (advance to next if at boundary, same logic as single-cluster path)
                let write_cluster = if self.context.offset % cluster_size == 0 {
                    // At cluster boundary - get next cluster from chain
                    match self.context.current_cluster {
                        None => self.context.first_cluster,
                        Some(n) => {
                            let r = self.fs.cluster_iter(n).next().await;
                            match r {
                                Some(Err(err)) => return Err(err),
                                Some(Ok(next)) => Some(next),
                                None => None,  // End of chain - fall through to single-cluster to allocate
                            }
                        }
                    }
                } else {
                    self.context.current_cluster
                };

                // Only attempt multi-cluster write if we have an allocated cluster
                if let Some(current_cluster) = write_cluster {
                    trace!("attempting multi-cluster write");
                    match crate::multi_cluster_io::write_contiguous(
                        self.fs,
                        current_cluster,
                        offset_in_cluster,
                        buf,
                    )
                    .await
                    {
                    Ok(written_bytes) if written_bytes > 0 => {
                        // Multi-cluster write succeeded!
                        let written_bytes = cmp::min(written_bytes, bytes_left_until_max_file_size);

                        let old_offset = self.context.offset;
                        self.context.offset += written_bytes as u32;
                        let new_offset = self.context.offset;

                        // Update current cluster to match new offset
                        // FAT convention: when at a cluster boundary, current_cluster points to
                        // the previous cluster (the one just finished), not the next cluster.
                        let old_cluster_index = if old_offset == 0 {
                            0u32
                        } else {
                            (old_offset / cluster_size).saturating_sub(1)
                        };

                        let new_cluster_index = if new_offset > 0 && new_offset % cluster_size == 0 {
                            (new_offset / cluster_size).saturating_sub(1)
                        } else {
                            new_offset / cluster_size
                        };

                        let cluster_delta = new_cluster_index.saturating_sub(old_cluster_index);

                        if cluster_delta > 0 {
                            let mut cluster = current_cluster;
                            for _i in 0..cluster_delta {
                                let mut iter = self.fs.cluster_iter(cluster);
                                if let Some(Ok(next)) = iter.next().await {
                                    cluster = next;
                                    // Record checkpoint during sequential write traversal
                                    #[cfg(feature = "cluster-checkpoints")]
                                    {
                                        let cluster_idx = old_cluster_index + _i + 1;
                                        self.record_checkpoint(cluster_idx, cluster);
                                    }
                                } else {
                                    break;
                                }
                            }
                            self.context.current_cluster = Some(cluster);
                        }

                        self.update_dir_entry_after_write();
                        trace!("multi-cluster write: {} bytes", written_bytes);
                        return Ok(written_bytes);
                    }
                        _ => {
                            // Fall through to single-cluster write
                            trace!("falling back to single-cluster write");
                        }
                    }
                }
            }
        }

        // Original single-cluster write path
        let bytes_left_in_cluster = (cluster_size - offset_in_cluster) as usize;
        let write_size = cmp::min(buf.len(), bytes_left_in_cluster);
        let write_size = cmp::min(write_size, bytes_left_until_max_file_size);

        if write_size == 0 {
            return Ok(0);
        }
        // Get cluster for write possibly allocating new one
        let current_cluster = if self.context.offset % cluster_size == 0 {
            // next cluster
            let next_cluster = match self.context.current_cluster {
                None => self.context.first_cluster,
                Some(n) => {
                    let r = self.fs.cluster_iter(n).next().await;
                    match r {
                        Some(Err(err)) => return Err(err),
                        Some(Ok(n)) => Some(n),
                        None => None,
                    }
                }
            };
            if let Some(n) = next_cluster {
                n
            } else {
                // end of chain reached - allocate new cluster
                let new_cluster = self
                    .fs
                    .alloc_cluster(self.context.current_cluster, self.is_dir())
                    .await?;
                trace!("allocated cluster {}", new_cluster);
                if self.context.first_cluster.is_none() {
                    self.set_first_cluster(new_cluster);
                }
                new_cluster
            }
        } else {
            // self.context.current_cluster should be a valid cluster
            match self.context.current_cluster {
                Some(n) => n,
                None => panic!("Offset inside cluster but no cluster allocated"),
            }
        };
        trace!("write {} bytes in cluster {}", write_size, current_cluster);
        let offset_in_fs = self.fs.offset_from_cluster(current_cluster) + u64::from(offset_in_cluster);
        #[allow(clippy::await_holding_refcell_ref)]
        let written_bytes = {
            let mut disk = self.fs.disk.lock().await;
            disk.seek(SeekFrom::Start(offset_in_fs)).await?;
            disk.write(&buf[..write_size]).await?
        };
        if written_bytes == 0 {
            return Ok(0);
        }
        // some bytes were writter - update position and optionally size
        self.context.offset += written_bytes as u32;
        self.context.current_cluster = Some(current_cluster);

        // Record checkpoint for sequential writes
        #[cfg(feature = "cluster-checkpoints")]
        if self.context.offset > 0 {
            let cluster_idx = (self.context.offset / cluster_size).saturating_sub(1);
            self.record_checkpoint(cluster_idx, current_cluster);
        }

        self.update_dir_entry_after_write();
        Ok(written_bytes)
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        Self::flush(self).await
    }
}

impl<IO: ReadWriteSeek, TP, OCC> Seek for File<'_, IO, TP, OCC> {
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        trace!("File::seek");
        let size_opt = self.size();
        let new_offset_opt: Option<u32> = match pos {
            SeekFrom::Current(x) => i64::from(self.context.offset)
                .checked_add(x)
                .and_then(|n| u32::try_from(n).ok()),
            SeekFrom::Start(x) => u32::try_from(x).ok(),
            SeekFrom::End(o) => size_opt
                .and_then(|s| i64::from(s).checked_add(o))
                .and_then(|n| u32::try_from(n).ok()),
        };
        let Some(mut new_offset) = new_offset_opt else {
            error!("Invalid seek offset");
            return Err(Error::InvalidInput);
        };
        if let Some(size) = size_opt {
            if new_offset > size {
                warn!("Seek beyond the end of the file");
                new_offset = size;
            }
        }
        trace!(
            "file seek {} -> {} - entry {:?}",
            self.context.offset,
            new_offset,
            self.context.entry
        );
        if new_offset == self.context.offset {
            // position is the same - nothing to do
            return Ok(u64::from(self.context.offset));
        }
        let new_offset_in_clusters = self.fs.clusters_from_bytes(u64::from(new_offset));
        let old_offset_in_clusters = self.fs.clusters_from_bytes(u64::from(self.context.offset));
        let new_cluster = if new_offset == 0 {
            None
        } else if new_offset_in_clusters == old_offset_in_clusters {
            self.context.current_cluster
        } else if let Some(first_cluster) = self.context.first_cluster {
            // calculate number of clusters to skip
            // return the previous cluster if the offset points to the cluster boundary
            // Note: new_offset_in_clusters cannot be 0 here because new_offset is not 0
            debug_assert!(new_offset_in_clusters > 0);
            let clusters_to_skip = new_offset_in_clusters - 1;

            // Phase 3 Optimization: Use cluster chain checkpoints for O(log n) seeking
            #[cfg(feature = "cluster-checkpoints")]
            let (mut cluster, start_index) = self.find_closest_checkpoint(clusters_to_skip);

            #[cfg(not(feature = "cluster-checkpoints"))]
            let (mut cluster, start_index) = (first_cluster, 0);

            let mut iter = self.fs.cluster_iter(cluster);
            for i in start_index..clusters_to_skip {
                cluster = if let Some(r) = iter.next().await {
                    r?
                } else {
                    // cluster chain ends before the new position - seek to the end of the last cluster
                    new_offset = self.fs.bytes_from_clusters(i + 1) as u32;
                    break;
                };
            }
            Some(cluster)
        } else {
            // empty file - always seek to 0
            new_offset = 0;
            None
        };
        self.context.offset = new_offset;
        self.context.current_cluster = new_cluster;
        Ok(u64::from(self.context.offset))
    }
}
