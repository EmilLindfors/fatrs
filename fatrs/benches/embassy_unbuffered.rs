use embedded_io_async::{ErrorType, Read, Seek, SeekFrom, Write};
use futures::executor::block_on;
///! Embassy-Compatible Unbuffered I/O Benchmark
///!
///! This benchmark tests DIRECT, UNBUFFERED I/O as it would occur on SD cards, eMMC,
///! or flash storage in embedded systems (ESP32, STM32, RP2040, etc.)
///!
///! Uses standard async runtime but implements embedded-io-async traits to simulate
///! real embedded hardware access patterns.
///!
///! This is the TRUE test of fat-cache and multi-cluster-io optimizations.
use std::fs::File;
use std::io::{Read as StdRead, Seek as StdSeek, Write as StdWrite};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// Embassy-compatible I/O counting wrapper for std::fs::File
/// Simulates direct block device access with operation counting
///
/// This represents unbuffered SD card or eMMC access patterns typical
/// in embedded systems - every operation goes directly to storage.
#[derive(Clone)]
struct DirectBlockDevice {
    inner: Arc<Mutex<File>>,
    read_count: Arc<AtomicU64>,
    write_count: Arc<AtomicU64>,
    seek_count: Arc<AtomicU64>,
    bytes_read: Arc<AtomicU64>,
    bytes_written: Arc<AtomicU64>,
}

impl DirectBlockDevice {
    fn new(file: File) -> Self {
        Self {
            inner: Arc::new(Mutex::new(file)),
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

    fn avg_read_size(&self) -> f64 {
        if self.read_ops == 0 {
            0.0
        } else {
            self.bytes_read as f64 / self.read_ops as f64
        }
    }

    fn avg_write_size(&self) -> f64 {
        if self.write_ops == 0 {
            0.0
        } else {
            self.bytes_written as f64 / self.write_ops as f64
        }
    }
}

impl ErrorType for DirectBlockDevice {
    type Error = std::io::Error;
}

impl Read for DirectBlockDevice {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Self::Error> {
        self.read_count.fetch_add(1, Ordering::Relaxed);
        let mut file = self.inner.lock().unwrap();
        let result = file.read(buf);
        if let Ok(n) = result {
            self.bytes_read.fetch_add(n as u64, Ordering::Relaxed);
        }
        result
    }
}

impl Write for DirectBlockDevice {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Self::Error> {
        self.write_count.fetch_add(1, Ordering::Relaxed);
        let mut file = self.inner.lock().unwrap();
        let result = file.write(buf);
        if let Ok(n) = result {
            self.bytes_written.fetch_add(n as u64, Ordering::Relaxed);
        }
        result
    }

    async fn flush(&mut self) -> Result<(), Self::Error> {
        let mut file = self.inner.lock().unwrap();
        file.flush()
    }
}

impl Seek for DirectBlockDevice {
    async fn seek(&mut self, pos: SeekFrom) -> Result<u64, Self::Error> {
        self.seek_count.fetch_add(1, Ordering::Relaxed);
        let mut file = self.inner.lock().unwrap();
        let std_pos = match pos {
            SeekFrom::Start(n) => std::io::SeekFrom::Start(n),
            SeekFrom::End(n) => std::io::SeekFrom::End(n),
            SeekFrom::Current(n) => std::io::SeekFrom::Current(n),
        };
        file.seek(std_pos)
    }
}

fn main() {
    block_on(async_main());
}

async fn async_main() {
    println!("===== Embedded-FatFS Embassy Unbuffered Benchmark =====");
    println!("Simulating direct block device access (SD card/eMMC patterns)");

    #[cfg(feature = "fat-cache")]
    println!("✓ FAT cache ENABLED");
    #[cfg(not(feature = "fat-cache"))]
    println!("✗ FAT cache DISABLED");

    #[cfg(feature = "multi-cluster-io")]
    println!("✓ Multi-cluster I/O ENABLED");
    #[cfg(not(feature = "multi-cluster-io"))]
    println!("✗ Multi-cluster I/O DISABLED");

    println!();

    // Setup test image
    setup_test_image();

    // Run benchmarks
    benchmark_sequential_read().await;
    benchmark_sequential_write().await;
    benchmark_random_access().await;

    // Cleanup
    let _ = std::fs::remove_file("target/embassy_bench.img");
}

fn setup_test_image() {
    let _ = std::fs::create_dir_all("target");

    // Copy test image
    if let Err(_) = std::fs::copy(
        "embedded-fatfs/resources/fat32.img",
        "target/embassy_bench.img",
    ) {
        std::fs::copy("resources/fat32.img", "target/embassy_bench.img")
            .expect("Failed to copy test image");
    }
}

async fn benchmark_sequential_read() {
    println!("--- Sequential Read (Unbuffered Direct I/O) ---");

    let file = File::options()
        .read(true)
        .write(true)
        .open("target/embassy_bench.img")
        .unwrap();

    // Direct block device - NO buffering layer!
    let device = DirectBlockDevice::new(file);
    let fs = fatrs::FileSystem::new(device.clone(), fatrs::FsOptions::new())
        .await
        .unwrap();

    // Create test file
    let test_data = vec![0xAA; 1024 * 1024]; // 1MB
    let mut file = fs.root_dir().create_file("bench_read.bin").await.unwrap();

    // Write 5MB
    for _ in 0..5 {
        file.write_all(&test_data).await.unwrap();
    }
    file.flush().await.unwrap();
    drop(file);

    // Reset counters
    device.reset_statistics();

    // Benchmark
    let mut file = fs.root_dir().open_file("bench_read.bin").await.unwrap();
    let mut buf = vec![0u8; 1024 * 1024]; // 1MB reads
    let mut total_read = 0u64;

    let start = Instant::now();

    loop {
        match file.read(&mut buf).await {
            Ok(0) => break,
            Ok(n) => total_read += n as u64,
            Err(_) => break,
        }
    }

    let elapsed = start.elapsed();
    let stats = device.statistics();

    // Get cache statistics if available
    #[cfg(feature = "fat-cache")]
    let cache_stats = fs.fat_cache_statistics().await;

    print_sequential_results("READ", total_read, elapsed, stats);

    #[cfg(feature = "fat-cache")]
    {
        println!("  FAT Cache Statistics:");
        println!("    Hits: {}", cache_stats.hits);
        println!("    Misses: {}", cache_stats.misses);
        println!("    Hit rate: {:.1}%", cache_stats.hit_rate * 100.0);
        println!();
    }
}

async fn benchmark_sequential_write() {
    println!("--- Sequential Write (Unbuffered Direct I/O) ---");

    let file = File::options()
        .read(true)
        .write(true)
        .open("target/embassy_bench.img")
        .unwrap();

    let device = DirectBlockDevice::new(file);
    let fs = fatrs::FileSystem::new(device.clone(), fatrs::FsOptions::new())
        .await
        .unwrap();

    device.reset_statistics();

    let mut file = fs.root_dir().create_file("bench_write.bin").await.unwrap();
    let chunk = vec![0xBB; 1024 * 1024]; // 1MB
    let chunks = 5;

    let start = Instant::now();

    for _ in 0..chunks {
        file.write_all(&chunk).await.unwrap();
    }
    file.flush().await.unwrap();

    let elapsed = start.elapsed();
    let stats = device.statistics();
    let total = chunks * 1024 * 1024;

    // Get cache statistics if available
    #[cfg(feature = "fat-cache")]
    let cache_stats = fs.fat_cache_statistics().await;

    print_sequential_results("WRITE", total, elapsed, stats);

    #[cfg(feature = "fat-cache")]
    {
        println!("  FAT Cache Statistics:");
        println!("    Hits: {}", cache_stats.hits);
        println!("    Misses: {}", cache_stats.misses);
        println!("    Hit rate: {:.1}%", cache_stats.hit_rate * 100.0);
        println!();
    }
}

async fn benchmark_random_access() {
    println!("--- Random Access (Unbuffered Direct I/O) ---");

    let file = File::options()
        .read(true)
        .write(true)
        .open("target/embassy_bench.img")
        .unwrap();

    let device = DirectBlockDevice::new(file);
    let fs = fatrs::FileSystem::new(device.clone(), fatrs::FsOptions::new())
        .await
        .unwrap();

    // Create 10MB test file
    let test_data = vec![0xCD; 1024 * 1024];
    let mut file = fs.root_dir().create_file("random_test.bin").await.unwrap();
    for _ in 0..10 {
        file.write_all(&test_data).await.unwrap();
    }
    file.flush().await.unwrap();
    drop(file);

    device.reset_statistics();

    // Random access benchmark
    let mut file = fs.root_dir().open_file("random_test.bin").await.unwrap();
    let mut buf = vec![0u8; 4096];
    let iterations = 100u32;
    let file_size = 10 * 1024 * 1024u64;

    let start = Instant::now();

    for i in 0..iterations {
        let offset = ((i as u64 * 12345) % (file_size / 4096)) * 4096;
        file.seek(SeekFrom::Start(offset)).await.unwrap();
        file.read(&mut buf).await.unwrap();
    }

    let elapsed = start.elapsed();
    let stats = device.statistics();
    let avg_latency = elapsed / iterations;

    // Get cache statistics if available
    #[cfg(feature = "fat-cache")]
    let cache_stats = fs.fat_cache_statistics().await;

    println!("  Iterations: {}", iterations);
    println!("  Total time: {:.3}s", elapsed.as_secs_f64());
    println!("  Avg latency: {:.2}ms", avg_latency.as_secs_f64() * 1000.0);
    println!(
        "  Operations/sec: {:.0}",
        iterations as f64 / elapsed.as_secs_f64()
    );
    println!();
    println!("  Storage I/O Statistics:");
    println!("    Read operations: {}", stats.read_ops);
    println!("    Write operations: {}", stats.write_ops);
    println!("    Seek operations: {}", stats.seek_ops);
    println!("    Total I/O ops: {}", stats.total_ops());
    println!(
        "    I/O ops per random read: {:.1}",
        stats.total_ops() as f64 / iterations as f64
    );
    println!();

    #[cfg(feature = "fat-cache")]
    {
        println!("  FAT Cache Statistics:");
        println!("    Hits: {}", cache_stats.hits);
        println!("    Misses: {}", cache_stats.misses);
        println!("    Hit rate: {:.1}%", cache_stats.hit_rate * 100.0);
        println!(
            "    Cache effectiveness: Reduced FAT reads by {:.1}x",
            (cache_stats.hits + cache_stats.misses) as f64 / cache_stats.misses.max(1) as f64
        );
    }

    println!();
}

fn print_sequential_results(
    operation: &str,
    bytes: u64,
    elapsed: std::time::Duration,
    stats: IoStatistics,
) {
    let mb = bytes as f64 / 1_048_576.0;
    let throughput = mb / elapsed.as_secs_f64();

    println!("  Total {}: {:.2} MB", operation.to_lowercase(), mb);
    println!("  Time: {:.3}s", elapsed.as_secs_f64());
    println!("  Throughput: {:.2} MB/s", throughput);
    println!();
    println!("  Storage I/O Statistics:");
    println!("    Read operations: {}", stats.read_ops);
    println!("    Write operations: {}", stats.write_ops);
    println!("    Seek operations: {}", stats.seek_ops);
    println!("    Total I/O ops: {}", stats.total_ops());

    if operation == "READ" {
        println!("    Avg bytes/read: {:.0}", stats.avg_read_size());
        println!(
            "    Data read efficiency: {:.1}%",
            (stats.bytes_read as f64 / (stats.read_ops as f64 * 512.0)) * 100.0
        );
    } else {
        println!("    Avg bytes/write: {:.0}", stats.avg_write_size());
        if stats.write_ops > 0 {
            println!(
                "    Data write efficiency: {:.1}%",
                (stats.bytes_written as f64 / (stats.write_ops as f64 * 512.0)) * 100.0
            );
        }
    }

    #[cfg(feature = "multi-cluster-io")]
    println!("    (Multi-cluster I/O batches contiguous operations)");

    println!();
}
