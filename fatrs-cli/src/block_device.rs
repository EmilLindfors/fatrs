//! Block device adapter for tokio files
//!
//! Provides a `BlockDevice<512>` implementation wrapping tokio async files.

use aligned::{A4, Aligned};
use fatrs_block_device::BlockDevice;
use embedded_io_async::{ErrorType, Read, Seek, SeekFrom, Write};

const BLOCK_SIZE: usize = 512;

/// Block device wrapper for async I/O streams
///
/// Wraps any type implementing `embedded_io_async::{Read, Write, Seek}`
/// and provides the `BlockDevice<512>` trait.
pub struct StreamBlockDevice<T>(pub T);

impl<T: ErrorType> ErrorType for StreamBlockDevice<T> {
    type Error = T::Error;
}

impl<T> BlockDevice<BLOCK_SIZE> for StreamBlockDevice<T>
where
    T: Read + Write + Seek,
{
    type Error = T::Error;
    type Align = A4;

    async fn read(
        &mut self,
        block_address: u32,
        data: &mut [Aligned<Self::Align, [u8; BLOCK_SIZE]>],
    ) -> Result<(), Self::Error> {
        self.0
            .seek(SeekFrom::Start((block_address as u64) * BLOCK_SIZE as u64))
            .await?;
        for block in data {
            let mut offset = 0;
            while offset < BLOCK_SIZE {
                let n = self.0.read(&mut block[offset..]).await?;
                if n == 0 {
                    break; // EOF
                }
                offset += n;
            }
        }
        Ok(())
    }

    async fn write(
        &mut self,
        block_address: u32,
        data: &[Aligned<Self::Align, [u8; BLOCK_SIZE]>],
    ) -> Result<(), Self::Error> {
        self.0
            .seek(SeekFrom::Start((block_address as u64) * BLOCK_SIZE as u64))
            .await?;
        for block in data {
            let mut offset = 0;
            while offset < BLOCK_SIZE {
                let n = self.0.write(&block[offset..]).await?;
                if n == 0 {
                    break; // Can't write more
                }
                offset += n;
            }
        }
        Ok(())
    }

    async fn size(&mut self) -> Result<u64, Self::Error> {
        // Seek to end to get size
        let size = self.0.seek(SeekFrom::End(0)).await?;
        // Seek back to beginning
        self.0.seek(SeekFrom::Start(0)).await?;
        Ok(size)
    }
}
