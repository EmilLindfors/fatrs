//! Async I/O helper functions for handling partial reads and writes.
//!
//! These functions provide a convenient way to read/write exact amounts of data
//! when the underlying stream may return partial results.

use embedded_io_async::{Read, Write};

/// Read exactly `buf.len()` bytes, handling partial reads.
///
/// This function will repeatedly call `read()` until the buffer is completely
/// filled or an error occurs. Returns `Ok(())` on success.
///
/// # Errors
///
/// Returns the underlying read error if one occurs. Note that if the stream
/// reaches EOF before filling the buffer, this may result in an incomplete read
/// (the function will return Ok but the buffer may not be fully filled).
///
/// # Example
///
/// ```ignore
/// use fatrs_adapters_core::read_exact_async;
///
/// let mut buffer = [0u8; 512];
/// read_exact_async(&mut reader, &mut buffer).await?;
/// ```
pub async fn read_exact_async<R: Read>(reader: &mut R, buf: &mut [u8]) -> Result<(), R::Error> {
    let mut offset = 0;
    while offset < buf.len() {
        let n = reader.read(&mut buf[offset..]).await?;
        if n == 0 {
            // EOF reached before buffer filled
            break;
        }
        offset += n;
    }
    Ok(())
}

/// Write all bytes from `buf`, handling partial writes.
///
/// This function will repeatedly call `write()` until all data is written
/// or an error occurs. Returns `Ok(())` on success.
///
/// # Errors
///
/// Returns the underlying write error if one occurs. If a write returns 0
/// (which typically indicates the stream cannot accept more data), the
/// function will stop and return Ok.
///
/// # Example
///
/// ```ignore
/// use fatrs_adapters_core::write_all_async;
///
/// let data = b"Hello, World!";
/// write_all_async(&mut writer, data).await?;
/// ```
pub async fn write_all_async<W: Write>(writer: &mut W, buf: &[u8]) -> Result<(), W::Error> {
    let mut offset = 0;
    while offset < buf.len() {
        let n = writer.write(&buf[offset..]).await?;
        if n == 0 {
            // Cannot write more data
            break;
        }
        offset += n;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use embedded_io_async::ErrorType;

    /// A mock reader that returns data in chunks
    struct ChunkedReader {
        data: Vec<u8>,
        pos: usize,
        chunk_size: usize,
    }

    impl ChunkedReader {
        fn new(data: Vec<u8>, chunk_size: usize) -> Self {
            Self {
                data,
                pos: 0,
                chunk_size,
            }
        }
    }

    impl ErrorType for ChunkedReader {
        type Error = core::convert::Infallible;
    }

    impl Read for ChunkedReader {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
            let remaining = self.data.len() - self.pos;
            if remaining == 0 {
                return Ok(0);
            }
            let to_read = buf.len().min(self.chunk_size).min(remaining);
            buf[..to_read].copy_from_slice(&self.data[self.pos..self.pos + to_read]);
            self.pos += to_read;
            Ok(to_read)
        }
    }

    /// A mock writer that accepts data in chunks
    struct ChunkedWriter {
        data: Vec<u8>,
        chunk_size: usize,
    }

    impl ChunkedWriter {
        fn new(chunk_size: usize) -> Self {
            Self {
                data: Vec::new(),
                chunk_size,
            }
        }
    }

    impl ErrorType for ChunkedWriter {
        type Error = core::convert::Infallible;
    }

    impl Write for ChunkedWriter {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
            let to_write = buf.len().min(self.chunk_size);
            self.data.extend_from_slice(&buf[..to_write]);
            Ok(to_write)
        }

        async fn flush(&mut self) -> Result<(), Self::Error> {
            Ok(())
        }
    }

    #[tokio::test]
    async fn test_read_exact_full_buffer() {
        let data = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let mut reader = ChunkedReader::new(data.clone(), 3); // Read 3 bytes at a time
        let mut buf = [0u8; 8];

        read_exact_async(&mut reader, &mut buf).await.unwrap();
        assert_eq!(&buf[..], &data[..]);
    }

    #[tokio::test]
    async fn test_read_exact_single_byte_chunks() {
        let data = vec![0xAA, 0xBB, 0xCC, 0xDD];
        let mut reader = ChunkedReader::new(data.clone(), 1); // Read 1 byte at a time
        let mut buf = [0u8; 4];

        read_exact_async(&mut reader, &mut buf).await.unwrap();
        assert_eq!(&buf[..], &data[..]);
    }

    #[tokio::test]
    async fn test_write_all_full_buffer() {
        let data = vec![1, 2, 3, 4, 5, 6, 7, 8];
        let mut writer = ChunkedWriter::new(3); // Write 3 bytes at a time

        write_all_async(&mut writer, &data).await.unwrap();
        assert_eq!(&writer.data[..], &data[..]);
    }

    #[tokio::test]
    async fn test_write_all_single_byte_chunks() {
        let data = vec![0xAA, 0xBB, 0xCC, 0xDD];
        let mut writer = ChunkedWriter::new(1); // Write 1 byte at a time

        write_all_async(&mut writer, &data).await.unwrap();
        assert_eq!(&writer.data[..], &data[..]);
    }
}
