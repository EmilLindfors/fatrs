//! Benchmark comparing different runtime configurations.
//!
//! This benchmark demonstrates the performance characteristics of:
//! - runtime-generic: Arc<async_lock::Mutex<T>>
//! - runtime-tokio: Arc<tokio::sync::Mutex<T>>
//! - alloc-only: Rc<RefCell<T>> (when no runtime features enabled)
//!
//! Run with:
//! ```sh
//! # Default (runtime-generic)
//! cargo bench --bench runtime_comparison
//!
//! # With runtime-tokio
//! cargo bench --bench runtime_comparison --no-default-features --features std,alloc,lfn,runtime-tokio
//!
//! # With alloc-only (Rc<RefCell>)
//! cargo bench --bench runtime_comparison --no-default-features --features alloc,lfn
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

use fatrs::share::Shared;

/// Measure the overhead of acquiring a lock on Shared<T>
fn bench_shared_acquire(iterations: u32) -> Duration {
    let shared = Shared::new(0u64);

    let start = Instant::now();

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        for _ in 0..iterations {
            #[cfg(any(feature = "runtime-tokio", feature = "runtime-generic"))]
            {
                let mut guard = shared.acquire().await;
                *guard = black_box(*guard + 1);
            }

            #[cfg(all(
                feature = "alloc",
                not(any(feature = "runtime-tokio", feature = "runtime-generic"))
            ))]
            {
                let mut guard = shared.acquire().await;
                *guard = black_box(*guard + 1);
            }
        }
    });

    start.elapsed()
}

/// Measure contention: multiple tasks acquiring the same lock
#[cfg(any(feature = "runtime-tokio", feature = "runtime-generic"))]
async fn bench_shared_contention_impl(tasks: u32, iterations_per_task: u32) -> Duration {
    let shared = Shared::new(0u64);
    let start = Instant::now();

    let mut handles = vec![];

    for _ in 0..tasks {
        let shared_clone = shared.clone();
        let handle = tokio::spawn(async move {
            for _ in 0..iterations_per_task {
                let mut guard = shared_clone.acquire().await;
                *guard = black_box(*guard + 1);
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.await.unwrap();
    }

    start.elapsed()
}

#[cfg(any(feature = "runtime-tokio", feature = "runtime-generic"))]
fn bench_shared_contention(tasks: u32, iterations_per_task: u32) -> Duration {
    let rt = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(4)
        .build()
        .unwrap();

    rt.block_on(bench_shared_contention_impl(tasks, iterations_per_task))
}

/// Measure throughput: operations per second
fn bench_throughput(duration_secs: u64) -> u64 {
    let shared = Shared::new(0u64);
    let target_duration = Duration::from_secs(duration_secs);

    let rt = tokio::runtime::Runtime::new().unwrap();
    rt.block_on(async {
        let start = Instant::now();
        let mut count = 0u64;

        while start.elapsed() < target_duration {
            #[cfg(any(feature = "runtime-tokio", feature = "runtime-generic"))]
            {
                let mut guard = shared.acquire().await;
                *guard = black_box(*guard + 1);
            }

            #[cfg(all(
                feature = "alloc",
                not(any(feature = "runtime-tokio", feature = "runtime-generic"))
            ))]
            {
                let mut guard = shared.acquire().await;
                *guard = black_box(*guard + 1);
            }

            count += 1;
        }

        count
    })
}

fn main() {
    println!("=== Shared<T> Runtime Performance Benchmark ===\n");

    // Detect which runtime is being used
    #[cfg(feature = "runtime-tokio")]
    println!("Runtime: tokio (Arc<tokio::sync::Mutex<T>>)");

    #[cfg(all(feature = "runtime-generic", not(feature = "runtime-tokio")))]
    println!("Runtime: generic (Arc<async_lock::Mutex<T>>)");

    #[cfg(all(
        feature = "alloc",
        not(any(feature = "runtime-tokio", feature = "runtime-generic"))
    ))]
    println!("Runtime: alloc-only (Rc<RefCell<T>>)");

    println!();

    // Benchmark 1: Single-threaded acquire overhead
    println!("Benchmark 1: Single-threaded lock acquisition");
    println!("  Iterations: 1,000,000");

    let duration = bench_shared_acquire(1_000_000);
    let nanos_per_op = duration.as_nanos() / 1_000_000;

    println!("  Total time: {:?}", duration);
    println!("  Time per operation: {} ns", nanos_per_op);
    println!();

    // Benchmark 2: Contention (only for Arc-based runtimes)
    #[cfg(any(feature = "runtime-tokio", feature = "runtime-generic"))]
    {
        println!("Benchmark 2: Multi-threaded contention");
        println!("  Tasks: 4");
        println!("  Iterations per task: 100,000");

        let duration = bench_shared_contention(4, 100_000);
        let total_ops = 4 * 100_000;
        let nanos_per_op = duration.as_nanos() / total_ops;

        println!("  Total time: {:?}", duration);
        println!("  Time per operation: {} ns", nanos_per_op);
        println!();
    }

    #[cfg(all(
        feature = "alloc",
        not(any(feature = "runtime-tokio", feature = "runtime-generic"))
    ))]
    {
        println!("Benchmark 2: Multi-threaded contention");
        println!("  SKIPPED (Rc<RefCell> is !Send)");
        println!();
    }

    // Benchmark 3: Throughput
    println!("Benchmark 3: Operations throughput (1 second)");

    let ops = bench_throughput(1);

    println!("  Total operations: {}", ops);
    println!("  Operations per second: {}", ops);
    println!("  Average time per op: {} ns", 1_000_000_000 / ops);
    println!();

    println!("=== Summary ===");

    #[cfg(feature = "runtime-tokio")]
    println!("tokio runtime provides optimal performance on tokio executor");

    #[cfg(all(feature = "runtime-generic", not(feature = "runtime-tokio")))]
    println!("async-lock is portable and works across all executors");

    #[cfg(all(
        feature = "alloc",
        not(any(feature = "runtime-tokio", feature = "runtime-generic"))
    ))]
    println!("Rc<RefCell> provides lowest overhead for single-threaded contexts");

    println!("\nRun this benchmark with different feature flags to compare!");
}
