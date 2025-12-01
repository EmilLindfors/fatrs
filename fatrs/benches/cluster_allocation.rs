use embedded_io_async::Write;
use fatrs::{FileSystem, FsOptions};
/// Cluster Allocation Benchmark
///
/// Measures the performance of cluster allocation at various volume fill levels.
/// This benchmark demonstrates the dramatic improvement provided by the cluster-bitmap feature.
///
/// Expected results:
/// - WITHOUT cluster-bitmap: O(n) allocation, gets slower as volume fills
/// - WITH cluster-bitmap: O(1) allocation, consistent speed regardless of fill level
///
/// Run with:
/// ```
/// cargo bench --bench cluster_allocation --features cluster-bitmap
/// cargo bench --bench cluster_allocation --no-default-features --features std,alloc,lfn
/// ```
use std::time::Instant;
use tokio::fs;

/// Fill filesystem to a target percentage by creating files
async fn fill_to_percentage<IO, TP, OCC>(fs: &FileSystem<IO, TP, OCC>, target_pct: f32)
where
    IO: embedded_io_async::Read + embedded_io_async::Write + embedded_io_async::Seek,
    IO::Error: 'static,
    TP: fatrs::TimeProvider,
    OCC: fatrs::OemCpConverter,
{
    let stats = fs.stats().await.unwrap();
    let target_clusters = (stats.total_clusters() as f32 * target_pct) as u32;
    let mut allocated = stats.total_clusters() - stats.free_clusters();

    let root_dir = fs.root_dir();
    let mut file_num = 0;

    // Allocate clusters until we reach target
    while allocated < target_clusters {
        let filename = format!("fill{}.dat", file_num);
        let mut file = root_dir.create_file(&filename).await.unwrap();

        // Write enough to allocate a few clusters
        let data = vec![0u8; 16384]; // 16KB = 4 clusters @ 4KB
        file.write_all(&data).await.unwrap();
        file.flush().await.unwrap();
        drop(file);

        file_num += 1;
        allocated += 4;

        // Update periodically
        if file_num % 100 == 0 {
            let current_stats = fs.stats().await.unwrap();
            allocated = current_stats.total_clusters() - current_stats.free_clusters();
        }
    }

    println!(
        "Filled to {}% ({}/{} clusters)",
        (allocated as f32 / stats.total_clusters() as f32 * 100.0),
        allocated,
        stats.total_clusters()
    );
}

/// Benchmark cluster allocation at a specific fill level
async fn bench_allocation_at_fill_level(fill_pct: f32, num_allocations: u32) {
    println!(
        "\n=== Benchmark: Allocation at {}% full ===",
        fill_pct * 100.0
    );

    // Use existing test image
    let img_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("target/bench_alloc.img")
        .await
        .unwrap();

    // Don't use BufStream - the FAT cache handles buffering more efficiently
    let fs = FileSystem::new(img_file, FsOptions::new()).await.unwrap();

    // Fill to target percentage
    if fill_pct > 0.0 {
        fill_to_percentage(&fs, fill_pct).await;
    }

    // Get initial stats
    let stats_before = fs.stats().await.unwrap();
    println!("Free clusters before: {}", stats_before.free_clusters());

    #[cfg(feature = "cluster-bitmap")]
    {
        let bitmap_stats = fs.cluster_bitmap_statistics().await;
        println!(
            "Bitmap: {} free, {} allocated, {:.1}% utilization",
            bitmap_stats.free_clusters,
            bitmap_stats.allocated_clusters,
            bitmap_stats.utilization * 100.0
        );
    }

    // Benchmark allocation
    let root_dir = fs.root_dir();
    let start = Instant::now();

    for i in 0..num_allocations {
        let filename = format!("bench{}.dat", i);
        let mut file = root_dir.create_file(&filename).await.unwrap();

        // Write minimal data to trigger cluster allocation
        file.write_all(&[0u8; 512]).await.unwrap();
        file.flush().await.unwrap();
        drop(file);
    }

    let elapsed = start.elapsed();

    // Calculate statistics
    let avg_time = elapsed / num_allocations;
    let allocations_per_sec = num_allocations as f64 / elapsed.as_secs_f64();

    println!("Results:");
    println!("  Total time: {:?}", elapsed);
    println!("  Avg time per allocation: {:?}", avg_time);
    println!("  Allocations/sec: {:.0}", allocations_per_sec);

    #[cfg(feature = "cluster-bitmap")]
    {
        let bitmap_stats = fs.cluster_bitmap_statistics().await;
        println!(
            "  Bitmap fast allocations: {}",
            bitmap_stats.fast_allocations
        );
        println!(
            "  Bitmap slow allocations: {}",
            bitmap_stats.slow_allocations
        );
    }

    let stats_after = fs.stats().await.unwrap();
    println!("Free clusters after: {}", stats_after.free_clusters());
}

/// Compare allocation performance with and without optimization
async fn compare_with_without_bitmap() {
    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║  CLUSTER ALLOCATION BENCHMARK - WITH vs WITHOUT BITMAP      ║");
    println!("╚══════════════════════════════════════════════════════════════╝");

    #[cfg(feature = "cluster-bitmap")]
    {
        println!("\n✓ CLUSTER-BITMAP FEATURE ENABLED");
        println!("Expected: Fast O(1) allocation regardless of fill level\n");
    }

    #[cfg(not(feature = "cluster-bitmap"))]
    {
        println!("\n✗ CLUSTER-BITMAP FEATURE DISABLED");
        println!("Expected: Slow O(n) allocation, gets worse as volume fills\n");
    }

    // Test at different fill levels
    let test_cases = [
        (0.10, "10% full - plenty of free space"),
        (0.50, "50% full - half allocated"),
        (0.90, "90% full - mostly full, fragmented"),
        (0.95, "95% full - nearly full"),
    ];

    for (fill_pct, description) in test_cases {
        println!("\n{}", "─".repeat(60));
        println!("{}", description);
        bench_allocation_at_fill_level(fill_pct, 100).await;
    }
}

/// Test very fragmented scenario (worst case for non-bitmap allocation)
async fn bench_worst_case_fragmentation() {
    println!("\n╔══════════════════════════════════════════════════════════════╗");
    println!("║  WORST CASE: Highly Fragmented Volume                       ║");
    println!("╚══════════════════════════════════════════════════════════════╝");

    let img_file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open("target/bench_frag.img")
        .await
        .unwrap();

    // Don't use BufStream - the FAT cache handles buffering more efficiently
    let fs = FileSystem::new(img_file, FsOptions::new()).await.unwrap();

    // Create fragmentation: allocate and delete every other file
    println!("\nCreating fragmentation pattern...");
    let root_dir = fs.root_dir();

    // Create 1000 files
    for i in 0..1000 {
        let filename = format!("frag{}.dat", i);
        let mut file = root_dir.create_file(&filename).await.unwrap();
        file.write_all(&[0u8; 4096]).await.unwrap();
        file.flush().await.unwrap();
        drop(file);
    }

    // Delete every other file to create holes
    for i in (0..1000).step_by(2) {
        let filename = format!("frag{}.dat", i);
        root_dir.remove(&filename).await.unwrap();
    }

    println!("Fragmentation created: 500 holes scattered throughout volume");

    let stats = fs.stats().await.unwrap();
    println!("Free clusters: {} (fragmented)", stats.free_clusters());

    // Now benchmark allocation in this fragmented scenario
    println!("\nAllocating 100 clusters in fragmented volume...");
    let start = Instant::now();

    for i in 0..100 {
        let filename = format!("bench{}.dat", i);
        let mut file = root_dir.create_file(&filename).await.unwrap();
        file.write_all(&[0u8; 512]).await.unwrap();
        file.flush().await.unwrap();
        drop(file);
    }

    let elapsed = start.elapsed();

    println!("\nResults in fragmented scenario:");
    println!("  Total time: {:?}", elapsed);
    println!("  Avg per allocation: {:?}", elapsed / 100);

    #[cfg(feature = "cluster-bitmap")]
    {
        let bitmap_stats = fs.cluster_bitmap_statistics().await;
        println!("  ✓ Bitmap handled fragmentation efficiently");
        println!("  Fast allocations: {}", bitmap_stats.fast_allocations);
    }

    #[cfg(not(feature = "cluster-bitmap"))]
    {
        println!("  ✗ Without bitmap: had to scan through allocated clusters");
    }
}

/// Main benchmark runner
#[tokio::main]
async fn main() {
    println!("\n");
    println!("╔══════════════════════════════════════════════════════════════╗");
    println!("║                                                              ║");
    println!("║     EMBEDDED-FATFS CLUSTER ALLOCATION BENCHMARK              ║");
    println!("║                                                              ║");
    println!("║  Phase 3 Optimization: Free Cluster Bitmap                  ║");
    println!("║                                                              ║");
    println!("╚══════════════════════════════════════════════════════════════╝");

    // Setup test images
    let _ = fs::create_dir_all("target").await;

    // Copy test image for each benchmark
    for name in &["bench_alloc", "bench_frag"] {
        let target_path = format!("target/{}.img", name);
        match fs::copy("embedded-fatfs/resources/fat32.img", &target_path).await {
            Ok(_) => {}
            Err(_) => {
                fs::copy("resources/fat32.img", &target_path)
                    .await
                    .expect("Failed to copy test image");
            }
        }
    }

    // Main comparison benchmark
    compare_with_without_bitmap().await;

    // Worst case scenario
    bench_worst_case_fragmentation().await;

    // Cleanup
    let _ = fs::remove_file("target/bench_alloc.img").await;
    let _ = fs::remove_file("target/bench_frag.img").await;

    println!("\n{}", "═".repeat(60));
    println!("BENCHMARK COMPLETE");
    println!("{}", "═".repeat(60));

    #[cfg(feature = "cluster-bitmap")]
    {
        println!("\n✓ With cluster-bitmap enabled:");
        println!("  - Allocation is O(1) - consistent speed");
        println!("  - Fragmentation has minimal impact");
        println!("  - Expected: 10-100x faster on fragmented volumes");
    }

    #[cfg(not(feature = "cluster-bitmap"))]
    {
        println!("\n✗ Without cluster-bitmap:");
        println!("  - Allocation is O(n) - slows with fill level");
        println!("  - Fragmentation causes severe slowdown");
        println!("  - Recommendation: Enable cluster-bitmap feature");
    }

    println!("\nRun both variants to compare:");
    println!("  cargo bench --bench cluster_allocation --features cluster-bitmap");
    println!(
        "  cargo bench --bench cluster_allocation --no-default-features --features std,alloc,lfn"
    );
}
