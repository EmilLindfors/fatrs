///! Page Buffer Benchmark
///!
///! Measures the throughput of different page buffer sizes for I/O operations.
///! This benchmark compares various page sizes (4KB to 1MB) to help determine
///! optimal buffer sizes for different storage types (HDD, SSD, NVMe).
use std::time::Instant;
use tokio::fs;

use aligned::{A4, Aligned};
use embedded_io_async::{ErrorType, Read, Seek, SeekFrom, Write};
use fatrs_adapters_alloc::{LargePageStream, presets};
use fatrs_adapters_core::PageStream;
use fatrs_block_device::BlockDevice;

const BLOCK_SIZE: usize = 512;

/// Test block device wrapping a file
struct FileBlockDevice<T>(T);

impl<T: ErrorType> ErrorType for FileBlockDevice<T> {
    type Error = T::Error;
}

impl<T> BlockDevice<BLOCK_SIZE> for FileBlockDevice<T>
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

        // Read all blocks in one contiguous read for better performance
        let total_bytes = data.len() * BLOCK_SIZE;
        let ptr = data.as_mut_ptr() as *mut u8;
        let buf = unsafe { core::slice::from_raw_parts_mut(ptr, total_bytes) };

        let mut offset = 0;
        while offset < total_bytes {
            let n = self.0.read(&mut buf[offset..]).await?;
            if n == 0 {
                break;
            }
            offset += n;
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

        // Write all blocks in one contiguous write for better performance
        let total_bytes = data.len() * BLOCK_SIZE;
        let ptr = data.as_ptr() as *const u8;
        let buf = unsafe { core::slice::from_raw_parts(ptr, total_bytes) };

        let mut offset = 0;
        while offset < total_bytes {
            let n = self.0.write(&buf[offset..]).await?;
            if n == 0 {
                break;
            }
            offset += n;
        }
        Ok(())
    }

    async fn size(&mut self) -> Result<u64, Self::Error> {
        let size = self.0.seek(SeekFrom::End(0)).await?;
        self.0.seek(SeekFrom::Start(0)).await?;
        Ok(size)
    }
}

/// Benchmark result
struct BenchResult {
    name: String,
    page_size: usize,
    operation: String,
    data_size_mb: f64,
    time_secs: f64,
    throughput_mb_s: f64,
}

impl BenchResult {
    fn print(&self) {
        println!(
            "  {:20} {:>8} | {:>6.2} MB in {:>6.3}s = {:>8.2} MB/s",
            self.name,
            format!("{}KB", self.page_size / 1024),
            self.data_size_mb,
            self.time_secs,
            self.throughput_mb_s
        );
    }
}

#[tokio::main]
async fn main() {
    println!("===== Page Buffer Size Benchmark =====\n");
    println!("This benchmark compares I/O throughput across different page buffer sizes.\n");

    // Create target directory
    let _ = fs::create_dir_all("target").await;

    // Test sizes (in bytes) - focus on key sizes
    let page_sizes = [
        ("4KB", presets::PAGE_4K),
        ("64KB", presets::PAGE_64K),
        ("128KB", presets::PAGE_128K),
        ("512KB", presets::PAGE_512K),
    ];

    // Run sequential write benchmarks
    println!("--- Sequential Write (2MB) ---");
    let mut write_results = Vec::new();
    for (name, size) in &page_sizes {
        let result = benchmark_sequential_write(*name, *size, 2).await;
        result.print();
        write_results.push(result);
    }
    println!();

    // Run sequential read benchmarks
    println!("--- Sequential Read (2MB) ---");
    let mut read_results = Vec::new();
    for (name, size) in &page_sizes {
        let result = benchmark_sequential_read(*name, *size, 2).await;
        result.print();
        read_results.push(result);
    }
    println!();

    // Run random access benchmarks (only small page sizes - larger ones are too slow)
    println!("--- Random Access (200 x 4KB blocks) ---");
    let random_sizes = [("4KB", presets::PAGE_4K), ("64KB", presets::PAGE_64K)];
    let mut random_results = Vec::new();
    for (name, size) in &random_sizes {
        let result = benchmark_random_access(*name, *size, 200).await;
        result.print();
        random_results.push(result);
    }
    println!();

    // Compare with stack-allocated PageStream (fixed sizes)
    println!("--- Stack-Allocated PageStream Comparison ---");
    benchmark_stack_page_stream().await;
    println!();

    // Summary
    println!("===== Summary =====\n");

    // Find best performers
    let best_write = write_results
        .iter()
        .max_by(|a, b| a.throughput_mb_s.partial_cmp(&b.throughput_mb_s).unwrap())
        .unwrap();
    let best_read = read_results
        .iter()
        .max_by(|a, b| a.throughput_mb_s.partial_cmp(&b.throughput_mb_s).unwrap())
        .unwrap();
    let best_random = random_results
        .iter()
        .max_by(|a, b| a.throughput_mb_s.partial_cmp(&b.throughput_mb_s).unwrap())
        .unwrap();

    println!(
        "Best Sequential Write: {} @ {:.2} MB/s",
        best_write.name, best_write.throughput_mb_s
    );
    println!(
        "Best Sequential Read:  {} @ {:.2} MB/s",
        best_read.name, best_read.throughput_mb_s
    );
    println!(
        "Best Random Access:    {} @ {:.2} MB/s",
        best_random.name, best_random.throughput_mb_s
    );
    println!();

    // Recommendations
    println!("Recommendations:");
    println!("  - For SSDs:  Use 128KB-256KB page sizes for optimal throughput");
    println!("  - For HDDs:  Use 64KB-128KB to balance seek time vs transfer size");
    println!("  - For NVMe:  Use 256KB-1MB to maximize queue depth benefits");
    println!("  - For random workloads: Smaller buffers (4KB-32KB) reduce wasted reads");

    // Cleanup
    let _ = fs::remove_file("target/page_bench_write.bin").await;
    let _ = fs::remove_file("target/page_bench_read.bin").await;
    let _ = fs::remove_file("target/page_bench_random.bin").await;
}

async fn benchmark_sequential_write(name: &str, page_size: usize, data_mb: usize) -> BenchResult {
    // Create a fresh file for writing
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open("target/page_bench_write.bin")
        .await
        .unwrap();

    // Pre-allocate file
    file.set_len((data_mb * 1024 * 1024) as u64).await.unwrap();

    let io = embedded_io_adapters::tokio_1::FromTokio::new(file);
    let block_dev = FileBlockDevice(io);
    let mut stream = LargePageStream::new(block_dev, page_size);

    let chunk = vec![0xAAu8; 64 * 1024]; // 64KB write chunks
    let total_bytes = data_mb * 1024 * 1024;
    let iterations = total_bytes / chunk.len();

    let start = Instant::now();

    for _ in 0..iterations {
        stream.write_all(&chunk).await.unwrap();
    }
    stream.flush().await.unwrap();

    let elapsed = start.elapsed();
    let throughput = (total_bytes as f64 / 1_048_576.0) / elapsed.as_secs_f64();

    BenchResult {
        name: name.to_string(),
        page_size,
        operation: "write".to_string(),
        data_size_mb: data_mb as f64,
        time_secs: elapsed.as_secs_f64(),
        throughput_mb_s: throughput,
    }
}

async fn benchmark_sequential_read(name: &str, page_size: usize, data_mb: usize) -> BenchResult {
    // Create a file with test data
    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open("target/page_bench_read.bin")
        .await
        .unwrap();

    let total_bytes = data_mb * 1024 * 1024;

    // Write test data first
    {
        let io = embedded_io_adapters::tokio_1::FromTokio::new(file);
        let block_dev = FileBlockDevice(io);
        let mut stream = LargePageStream::new(block_dev, page_size);

        let chunk = vec![0xBBu8; 64 * 1024];
        let iterations = total_bytes / chunk.len();

        for _ in 0..iterations {
            stream.write_all(&chunk).await.unwrap();
        }
        stream.flush().await.unwrap();
    }

    // Now benchmark reading
    let file = fs::OpenOptions::new()
        .read(true)
        .open("target/page_bench_read.bin")
        .await
        .unwrap();

    let io = embedded_io_adapters::tokio_1::FromTokio::new(file);
    let block_dev = FileBlockDevice(io);
    let mut stream = LargePageStream::new(block_dev, page_size);

    let mut buf = vec![0u8; 64 * 1024]; // 64KB read chunks
    let mut total_read = 0usize;

    let start = Instant::now();

    loop {
        let n = stream.read(&mut buf).await.unwrap();
        if n == 0 {
            break;
        }
        total_read += n;
    }

    let elapsed = start.elapsed();
    let throughput = (total_read as f64 / 1_048_576.0) / elapsed.as_secs_f64();

    BenchResult {
        name: name.to_string(),
        page_size,
        operation: "read".to_string(),
        data_size_mb: total_read as f64 / 1_048_576.0,
        time_secs: elapsed.as_secs_f64(),
        throughput_mb_s: throughput,
    }
}

async fn benchmark_random_access(name: &str, page_size: usize, num_accesses: usize) -> BenchResult {
    // Create a 64MB file for random access
    let file_size = 64 * 1024 * 1024u64;

    let file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(true)
        .open("target/page_bench_random.bin")
        .await
        .unwrap();

    file.set_len(file_size).await.unwrap();

    let io = embedded_io_adapters::tokio_1::FromTokio::new(file);
    let block_dev = FileBlockDevice(io);
    let mut stream = LargePageStream::new(block_dev, page_size);

    // Generate deterministic "random" offsets
    let mut offsets: Vec<u64> = (0..num_accesses)
        .map(|i| {
            // Simple hash-like distribution
            ((i as u64 * 2654435761) % (file_size - 4096)) & !0xFFF // 4KB aligned
        })
        .collect();
    offsets.sort(); // Sort for more predictable behavior, but still tests seeking

    let chunk_size = 4096; // 4KB reads
    let mut buf = vec![0u8; chunk_size];
    let mut total_read = 0usize;

    let start = Instant::now();

    for offset in &offsets {
        stream.seek(SeekFrom::Start(*offset)).await.unwrap();
        let n = stream.read(&mut buf).await.unwrap();
        total_read += n;
    }

    let elapsed = start.elapsed();
    let throughput = (total_read as f64 / 1_048_576.0) / elapsed.as_secs_f64();

    BenchResult {
        name: name.to_string(),
        page_size,
        operation: "random".to_string(),
        data_size_mb: total_read as f64 / 1_048_576.0,
        time_secs: elapsed.as_secs_f64(),
        throughput_mb_s: throughput,
    }
}

async fn benchmark_stack_page_stream() {
    println!("  Comparing heap-allocated LargePageStream vs stack-allocated PageStream");
    println!();

    // Test with 4KB pages (8 blocks)
    let data_mb = 2;

    // Stack-allocated PageStream<_, 8> (4KB)
    {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open("target/page_bench_stack.bin")
            .await
            .unwrap();

        file.set_len((data_mb * 1024 * 1024) as u64).await.unwrap();

        let io = embedded_io_adapters::tokio_1::FromTokio::new(file);
        let block_dev = FileBlockDevice(io);
        let mut stream: PageStream<_, 8> = PageStream::new(block_dev); // 8 * 512 = 4KB

        let chunk = vec![0xCCu8; 64 * 1024];
        let total_bytes = data_mb * 1024 * 1024;
        let iterations = total_bytes / chunk.len();

        let start = Instant::now();

        for _ in 0..iterations {
            stream.write_all(&chunk).await.unwrap();
        }
        stream.flush().await.unwrap();

        let elapsed = start.elapsed();
        let throughput = (total_bytes as f64 / 1_048_576.0) / elapsed.as_secs_f64();

        println!(
            "  PageStream<8> (stack, 4KB)    | {:>6.2} MB in {:>6.3}s = {:>8.2} MB/s",
            data_mb,
            elapsed.as_secs_f64(),
            throughput
        );

        let _ = fs::remove_file("target/page_bench_stack.bin").await;
    }

    // Stack-allocated PageStream<_, 16> (8KB)
    {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open("target/page_bench_stack.bin")
            .await
            .unwrap();

        file.set_len((data_mb * 1024 * 1024) as u64).await.unwrap();

        let io = embedded_io_adapters::tokio_1::FromTokio::new(file);
        let block_dev = FileBlockDevice(io);
        let mut stream: PageStream<_, 16> = PageStream::new(block_dev); // 16 * 512 = 8KB

        let chunk = vec![0xCCu8; 64 * 1024];
        let total_bytes = data_mb * 1024 * 1024;
        let iterations = total_bytes / chunk.len();

        let start = Instant::now();

        for _ in 0..iterations {
            stream.write_all(&chunk).await.unwrap();
        }
        stream.flush().await.unwrap();

        let elapsed = start.elapsed();
        let throughput = (total_bytes as f64 / 1_048_576.0) / elapsed.as_secs_f64();

        println!(
            "  PageStream<16> (stack, 8KB)   | {:>6.2} MB in {:>6.3}s = {:>8.2} MB/s",
            data_mb,
            elapsed.as_secs_f64(),
            throughput
        );

        let _ = fs::remove_file("target/page_bench_stack.bin").await;
    }

    // Heap-allocated LargePageStream (4KB for comparison)
    {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open("target/page_bench_stack.bin")
            .await
            .unwrap();

        file.set_len((data_mb * 1024 * 1024) as u64).await.unwrap();

        let io = embedded_io_adapters::tokio_1::FromTokio::new(file);
        let block_dev = FileBlockDevice(io);
        let mut stream = LargePageStream::new(block_dev, presets::PAGE_4K);

        let chunk = vec![0xCCu8; 64 * 1024];
        let total_bytes = data_mb * 1024 * 1024;
        let iterations = total_bytes / chunk.len();

        let start = Instant::now();

        for _ in 0..iterations {
            stream.write_all(&chunk).await.unwrap();
        }
        stream.flush().await.unwrap();

        let elapsed = start.elapsed();
        let throughput = (total_bytes as f64 / 1_048_576.0) / elapsed.as_secs_f64();

        println!(
            "  LargePageStream (heap, 4KB)   | {:>6.2} MB in {:>6.3}s = {:>8.2} MB/s",
            data_mb,
            elapsed.as_secs_f64(),
            throughput
        );

        let _ = fs::remove_file("target/page_bench_stack.bin").await;
    }

    // Heap-allocated LargePageStream (128KB)
    {
        let file = fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open("target/page_bench_stack.bin")
            .await
            .unwrap();

        file.set_len((data_mb * 1024 * 1024) as u64).await.unwrap();

        let io = embedded_io_adapters::tokio_1::FromTokio::new(file);
        let block_dev = FileBlockDevice(io);
        let mut stream = LargePageStream::new(block_dev, presets::PAGE_128K);

        let chunk = vec![0xCCu8; 64 * 1024];
        let total_bytes = data_mb * 1024 * 1024;
        let iterations = total_bytes / chunk.len();

        let start = Instant::now();

        for _ in 0..iterations {
            stream.write_all(&chunk).await.unwrap();
        }
        stream.flush().await.unwrap();

        let elapsed = start.elapsed();
        let throughput = (total_bytes as f64 / 1_048_576.0) / elapsed.as_secs_f64();

        println!(
            "  LargePageStream (heap, 128KB) | {:>6.2} MB in {:>6.3}s = {:>8.2} MB/s",
            data_mb,
            elapsed.as_secs_f64(),
            throughput
        );

        let _ = fs::remove_file("target/page_bench_stack.bin").await;
    }
}
