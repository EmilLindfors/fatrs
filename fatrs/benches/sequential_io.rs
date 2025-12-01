use embedded_io_async::{Read, Write};
///! Sequential I/O Benchmark
///!
///! Measures the throughput of sequential file reading and writing operations.
///! This benchmark demonstrates the performance improvements from FAT caching
///! and multi-cluster I/O optimizations.
use std::time::Instant;
use tokio::fs;

#[tokio::main]
async fn main() {
    println!("===== Embedded-FatFS Sequential I/O Benchmark =====\n");

    //Create target directory if it doesn't exist
    let _ = fs::create_dir_all("target").await;

    // Copy test image
    match fs::copy(
        "embedded-fatfs/resources/fat32.img",
        "target/bench_fat32.img",
    )
    .await
    {
        Ok(_) => {}
        Err(e) => {
            eprintln!(
                "Failed to copy from embedded-fatfs/resources/fat32.img: {}",
                e
            );
            // Try alternative path
            fs::copy("resources/fat32.img", "target/bench_fat32.img")
                .await
                .expect("Failed to copy test image from resources/fat32.img");
        }
    }

    // Benchmark sequential read
    benchmark_sequential_read().await;

    // Benchmark sequential write
    benchmark_sequential_write().await;

    // Cleanup
    let _ = fs::remove_file("target/bench_fat32.img").await;
}

async fn benchmark_sequential_read() {
    println!("--- Sequential Read Benchmark ---");

    let img_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("target/bench_fat32.img")
        .await
        .unwrap();

    // Don't use BufStream - the FAT cache handles buffering more efficiently
    let fs = fatrs::FileSystem::new(img_file, fatrs::FsOptions::new())
        .await
        .unwrap();

    // Create a test file first
    let test_data = vec![0xAA; 1024 * 1024]; // 1MB
    let mut file = fs.root_dir().create_file("bench_read.bin").await.unwrap();

    // Write 5MB test file
    for _ in 0..5 {
        file.write_all(&test_data).await.unwrap();
    }
    file.flush().await.unwrap();
    drop(file);

    // Now benchmark reading it
    let mut file = fs.root_dir().open_file("bench_read.bin").await.unwrap();
    let mut buf = vec![0u8; 1024 * 1024];
    let mut total_read = 0u64;

    let start = Instant::now();

    loop {
        match file.read(&mut buf).await {
            Ok(0) => break, // EOF
            Ok(n) => {
                total_read += n as u64;
            }
            Err(e) => {
                eprintln!("Read error: {:?}", e);
                break;
            }
        }
    }

    let elapsed = start.elapsed();
    let throughput_mb_s = (total_read as f64 / 1_048_576.0) / elapsed.as_secs_f64();

    println!("  Total read: {} MB", total_read / 1_048_576);
    println!("  Time: {:.3}s", elapsed.as_secs_f64());
    println!("  Throughput: {:.2} MB/s", throughput_mb_s);
    println!();
}

async fn benchmark_sequential_write() {
    println!("--- Sequential Write Benchmark ---");

    let img_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("target/bench_fat32.img")
        .await
        .unwrap();

    // Don't use BufStream - the FAT cache handles buffering more efficiently
    let fs = fatrs::FileSystem::new(img_file, fatrs::FsOptions::new())
        .await
        .unwrap();

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
    let total_written = chunks_to_write * 1024 * 1024;
    let throughput_mb_s = (total_written as f64 / 1_048_576.0) / elapsed.as_secs_f64();

    println!("  Total written: {} MB", total_written / 1_048_576);
    println!("  Time: {:.3}s", elapsed.as_secs_f64());
    println!("  Throughput: {:.2} MB/s", throughput_mb_s);
    println!();
}
