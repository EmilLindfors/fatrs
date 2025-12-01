use embedded_io_async::{ErrorType, Read, Seek, SeekFrom, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
///! Unbuffered I/O Benchmark
///!
///! This benchmark tests performance with DIRECT, UNBUFFERED I/O - the intended
///! use case for the fat-cache and multi-cluster-io optimizations.
///!
///! Unlike sequential_io.rs which uses BufStream, this benchmark directly wraps
///! the file to simulate embedded hardware access patterns (SD cards, eMMC, etc.)
use std::time::Instant;
use tokio::fs;

/// I/O counting wrapper to measure optimization effectiveness
/// Tracks actual storage operations, not application-level calls
#[derive(Clone)]
struct IoCountingFile {
    inner: Arc<tokio::sync::Mutex<tokio::fs::File>>,
    read_count: Arc<AtomicU64>,
    write_count: Arc<AtomicU64>,
    seek_count: Arc<AtomicU64>,
    bytes_read: Arc<AtomicU64>,
    bytes_written: Arc<AtomicU64>,
}

impl IoCountingFile {
    fn new(file: tokio::fs::File) -> Self {
        Self {
            inner: Arc::new(tokio::sync::Mutex::new(file)),
            read_count: Arc::new(AtomicU64::new(0)),
            write_count: Arc::new(AtomicU64::new(0)),
            seek_count: Arc::new(AtomicU64::new(0)),
            bytes_read: Arc::new(AtomicU64::new(0)),
            bytes_written: Arc::new(AtomicU64::new(0)),
        }
    }

    fn statistics(&self) -> IoStatistics {
        IoStatistics {
            read_ops: self.read_count.load(Ordering::Relaxed),
            write_ops: self.write_count.load(Ordering::Relaxed),
            seek_ops: self.seek_count.load(Ordering::Relaxed),
            bytes_read: self.bytes_read.load(Ordering::Relaxed),
            bytes_written: self.bytes_written.load(Ordering::Relaxed),
        }
    }

    fn reset_statistics(&self) {
        self.read_count.store(0, Ordering::Relaxed);
        self.write_count.store(0, Ordering::Relaxed);
        self.seek_count.store(0, Ordering::Relaxed);
        self.bytes_read.store(0, Ordering::Relaxed);
        self.bytes_written.store(0, Ordering::Relaxed);
    }
}

#[derive(Debug, Clone, Copy)]
struct IoStatistics {
    read_ops: u64,
    write_ops: u64,
    seek_ops: u64,
    bytes_read: u64,
    bytes_written: u64,
}

impl IoStatistics {
    fn total_ops(&self) -> u64 {
        self.read_ops + self.write_ops + self.seek_ops
    }
}

impl ErrorType for IoCountingFile {
    type Error = std::io::Error;
}

impl Read for IoCountingFile {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.read_count.fetch_add(1, Ordering::Relaxed);
        let mut file = self.inner.lock().await;
        let result = tokio::io::AsyncReadExt::read(&mut *file, buf).await;
        if let Ok(n) = result {
            self.bytes_read.fetch_add(n as u64, Ordering::Relaxed);
        }
        result
    }
}

impl Write for IoCountingFile {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.write_count.fetch_add(1, Ordering::Relaxed);
        let mut file = self.inner.lock().await;
        let result = tokio::io::AsyncWriteExt::write(&mut *file, buf).await;
        if let Ok(n) = result {
            self.bytes_written.fetch_add(n as u64, Ordering::Relaxed);
        }
        result
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        let mut file = self.inner.lock().await;
        tokio::io::AsyncWriteExt::flush(&mut *file).await
    }
}

impl Seek for IoCountingFile {
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        self.seek_count.fetch_add(1, Ordering::Relaxed);
        let mut file = self.inner.lock().await;
        let std_pos = match pos {
            SeekFrom::Start(n) => std::io::SeekFrom::Start(n),
            SeekFrom::End(n) => std::io::SeekFrom::End(n),
            SeekFrom::Current(n) => std::io::SeekFrom::Current(n),
        };
        tokio::io::AsyncSeekExt::seek(&mut *file, std_pos).await
    }
}

#[tokio::main]
async fn main() {
    println!("===== Embedded-FatFS Unbuffered I/O Benchmark =====");
    println!("Testing optimizations with direct I/O (no BufStream)\n");

    // Create target directory
    let _ = fs::create_dir_all("target").await;

    // Copy test image
    match fs::copy(
        "embedded-fatfs/resources/fat32.img",
        "target/bench_unbuffered.img",
    )
    .await
    {
        Ok(_) => {}
        Err(e) => {
            eprintln!(
                "Failed to copy from embedded-fatfs/resources/fat32.img: {}",
                e
            );
            fs::copy("resources/fat32.img", "target/bench_unbuffered.img")
                .await
                .expect("Failed to copy test image from resources/fat32.img");
        }
    }

    // Run benchmarks
    benchmark_unbuffered_sequential_read().await;
    benchmark_unbuffered_sequential_write().await;
    benchmark_unbuffered_random_access().await;

    // Cleanup
    let _ = fs::remove_file("target/bench_unbuffered.img").await;
}

async fn benchmark_unbuffered_sequential_read() {
    println!("--- Unbuffered Sequential Read Benchmark ---");

    let img_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("target/bench_unbuffered.img")
        .await
        .unwrap();

    // NO BufStream wrapping - direct I/O!
    let io_counter = IoCountingFile::new(img_file);
    let fs = fatrs::FileSystem::new(io_counter.clone(), fatrs::FsOptions::new())
        .await
        .unwrap();

    // Create a test file
    let test_data = vec![0xAA; 1024 * 1024]; // 1MB
    let mut file = fs.root_dir().create_file("bench_read.bin").await.unwrap();

    // Write 5MB test file
    for _ in 0..5 {
        file.write_all(&test_data).await.unwrap();
    }
    file.flush().await.unwrap();
    drop(file);

    // Reset I/O counters before actual benchmark
    io_counter.reset_statistics();

    // Benchmark reading
    let mut file = fs.root_dir().open_file("bench_read.bin").await.unwrap();
    let mut buf = vec![0u8; 1024 * 1024]; // 1MB buffer
    let mut total_read = 0u64;

    let start = Instant::now();

    loop {
        match file.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => total_read += n as u64,
            Err(e) => {
                eprintln!("Read error: {:?}", e);
                break;
            }
        }
    }

    let elapsed = start.elapsed();
    let stats = io_counter.statistics();
    let throughput_mb_s = (total_read as f64 / 1_048_576.0) / elapsed.as_secs_f64();

    println!("  Total read: {} MB", total_read / 1_048_576);
    println!("  Time: {:.3}s", elapsed.as_secs_f64());
    println!("  Throughput: {:.2} MB/s", throughput_mb_s);
    println!("  Storage I/O operations:");
    println!("    Reads: {}", stats.read_ops);
    println!("    Writes: {}", stats.write_ops);
    println!("    Seeks: {}", stats.seek_ops);
    println!("    Total ops: {}", stats.total_ops());
    println!(
        "    Avg bytes/read: {:.0}",
        stats.bytes_read as f64 / stats.read_ops as f64
    );
    println!();
}

async fn benchmark_unbuffered_sequential_write() {
    println!("--- Unbuffered Sequential Write Benchmark ---");

    let img_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("target/bench_unbuffered.img")
        .await
        .unwrap();

    let io_counter = IoCountingFile::new(img_file);
    let fs = fatrs::FileSystem::new(io_counter.clone(), fatrs::FsOptions::new())
        .await
        .unwrap();

    io_counter.reset_statistics();

    let mut file = fs.root_dir().create_file("bench_write.bin").await.unwrap();

    // Write 5MB in 1MB chunks
    let chunk = vec![0xBB; 1024 * 1024];
    let chunks_to_write = 5;

    let start = Instant::now();

    for _ in 0..chunks_to_write {
        file.write_all(&chunk).await.unwrap();
    }

    file.flush().await.unwrap();

    let elapsed = start.elapsed();
    let stats = io_counter.statistics();
    let total_written = chunks_to_write * 1024 * 1024;
    let throughput_mb_s = (total_written as f64 / 1_048_576.0) / elapsed.as_secs_f64();

    println!("  Total written: {} MB", total_written / 1_048_576);
    println!("  Time: {:.3}s", elapsed.as_secs_f64());
    println!("  Throughput: {:.2} MB/s", throughput_mb_s);
    println!("  Storage I/O operations:");
    println!("    Reads: {}", stats.read_ops);
    println!("    Writes: {}", stats.write_ops);
    println!("    Seeks: {}", stats.seek_ops);
    println!("    Total ops: {}", stats.total_ops());
    println!(
        "    Avg bytes/write: {:.0}",
        stats.bytes_written as f64 / stats.write_ops.max(1) as f64
    );
    println!();
}

async fn benchmark_unbuffered_random_access() {
    println!("--- Unbuffered Random Access Benchmark ---");

    let img_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("target/bench_unbuffered.img")
        .await
        .unwrap();

    let io_counter = IoCountingFile::new(img_file);
    let fs = fatrs::FileSystem::new(io_counter.clone(), fatrs::FsOptions::new())
        .await
        .unwrap();

    // Create a 10MB test file
    let test_data = vec![0xCD; 1024 * 1024];
    let mut file = fs.root_dir().create_file("random_test.bin").await.unwrap();

    for _ in 0..10 {
        file.write_all(&test_data).await.unwrap();
    }
    file.flush().await.unwrap();
    drop(file);

    // Reset counters before benchmark
    io_counter.reset_statistics();

    // Perform random reads
    let mut file = fs.root_dir().open_file("random_test.bin").await.unwrap();
    let mut buf = vec![0u8; 4096]; // 4KB reads

    let iterations = 100u32;
    let file_size = 10 * 1024 * 1024u64;

    let start = Instant::now();

    for i in 0..iterations {
        let offset = ((i as u64 * 12345) % (file_size / 4096)) * 4096;
        file.seek(SeekFrom::Start(offset)).await.unwrap();
        file.read(&mut buf).await.unwrap();
    }

    let elapsed = start.elapsed();
    let stats = io_counter.statistics();
    let avg_latency = elapsed / iterations;

    println!("  Iterations: {}", iterations);
    println!("  Total time: {:.3}s", elapsed.as_secs_f64());
    println!("  Avg latency: {:.2}ms", avg_latency.as_secs_f64() * 1000.0);
    println!(
        "  Operations/sec: {:.0}",
        iterations as f64 / elapsed.as_secs_f64()
    );
    println!("  Storage I/O operations:");
    println!("    Reads: {}", stats.read_ops);
    println!("    Writes: {}", stats.write_ops);
    println!("    Seeks: {}", stats.seek_ops);
    println!("    Total ops: {}", stats.total_ops());
    println!(
        "    I/O ops per random access: {:.1}",
        stats.total_ops() as f64 / iterations as f64
    );
    println!();
}
