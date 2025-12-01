use embedded_io_async::{Read, Seek, SeekFrom, Write};
///! Random Access Benchmark
///!
///! Measures the latency of random seek and read operations.
///! This benchmark demonstrates the effectiveness of FAT caching.
use std::time::Instant;
use tokio::fs;

#[tokio::main]
async fn main() {
    println!("===== Embedded-FatFS Random Access Benchmark =====\n");

    // Create target directory if it doesn't exist
    let _ = fs::create_dir_all("target").await;

    // Copy test image
    match fs::copy(
        "embedded-fatfs/resources/fat32.img",
        "target/bench_random.img",
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
            fs::copy("resources/fat32.img", "target/bench_random.img")
                .await
                .expect("Failed to copy test image from resources/fat32.img");
        }
    }

    benchmark_random_access().await;

    // Cleanup
    let _ = fs::remove_file("target/bench_random.img").await;
}

async fn benchmark_random_access() {
    println!("--- Random Access Latency Benchmark ---");

    let img_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("target/bench_random.img")
        .await
        .unwrap();

    // Don't use BufStream - the FAT cache handles buffering more efficiently
    let fs = fatrs::FileSystem::new(img_file, fatrs::FsOptions::new())
        .await
        .unwrap();

    // Create a 10MB test file
    let test_data = vec![0xCD; 1024 * 1024]; // 1MB chunks
    let mut file = fs.root_dir().create_file("random_test.bin").await.unwrap();

    for _ in 0..10 {
        file.write_all(&test_data).await.unwrap();
    }
    file.flush().await.unwrap();
    drop(file);

    // Now perform random reads
    let mut file = fs.root_dir().open_file("random_test.bin").await.unwrap();
    let mut buf = vec![0u8; 4096]; // 4KB reads

    let iterations = 100u32;
    let file_size = 10 * 1024 * 1024u64;

    let start = Instant::now();

    for i in 0..iterations {
        // Random offset (aligned to 4KB for consistency)
        let offset = ((i as u64 * 12345) % (file_size / 4096)) * 4096;

        file.seek(SeekFrom::Start(offset)).await.unwrap();
        file.read(&mut buf).await.unwrap();
    }

    let elapsed = start.elapsed();
    let avg_latency = elapsed / iterations;

    println!("  Iterations: {}", iterations);
    println!("  Total time: {:.3}s", elapsed.as_secs_f64());
    println!("  Avg latency: {:.2}ms", avg_latency.as_secs_f64() * 1000.0);
    println!(
        "  Operations/sec: {:.0}",
        iterations as f64 / elapsed.as_secs_f64()
    );
    println!();
}
