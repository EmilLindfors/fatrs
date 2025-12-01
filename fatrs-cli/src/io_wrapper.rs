//! IO wrapper to unify file and device access

use embedded_io_async::{ErrorType, Read, Seek, SeekFrom, Write};

#[cfg(windows)]
use fatrs_adapters_alloc::LargePageStream;
#[cfg(windows)]
use fatrs_block_platform::StreamBlockDevice;

/// Unified IO type that can be either a file or a Windows device
pub enum UnifiedIO {
    File(embedded_io_adapters::tokio_1::FromTokio<tokio::fs::File>),
    #[cfg(windows)]
    Device(LargePageStream<StreamBlockDevice<fatrs_cli::AsyncWindowsDevice>>),
}

impl ErrorType for UnifiedIO {
    type Error = std::io::Error;
}

impl Read for UnifiedIO {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        match self {
            UnifiedIO::File(f) => f.read(buf).await,
            #[cfg(windows)]
            UnifiedIO::Device(d) => d
                .read(buf)
                .await
                .map_err(|e| std::io::Error::other(format!("{:?}", e))),
        }
    }
}

impl Write for UnifiedIO {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        match self {
            UnifiedIO::File(f) => f.write(buf).await,
            #[cfg(windows)]
            UnifiedIO::Device(d) => d
                .write(buf)
                .await
                .map_err(|e| std::io::Error::other(format!("{:?}", e))),
        }
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        match self {
            UnifiedIO::File(f) => f.flush().await,
            #[cfg(windows)]
            UnifiedIO::Device(d) => d
                .flush()
                .await
                .map_err(|e| std::io::Error::other(format!("{:?}", e))),
        }
    }
}

impl Seek for UnifiedIO {
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        match self {
            UnifiedIO::File(f) => f.seek(pos).await,
            #[cfg(windows)]
            UnifiedIO::Device(d) => d
                .seek(pos)
                .await
                .map_err(|e| std::io::Error::other(format!("{:?}", e))),
        }
    }
}
