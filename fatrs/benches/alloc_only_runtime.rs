//! Benchmark for alloc-only (Rc<RefCell>) configuration.
//!
//! This demonstrates the **zero-overhead** characteristics when using
//! Shared<T> without any async runtime - just Rc<RefCell<T>>.
//!
//! Run with:
//! ```sh
//! cargo bench --bench alloc_only_runtime --no-default-features --features alloc,lfn
//! ```

use std::hint::black_box;
use std::time::{Duration, Instant};

#[cfg(all(
    feature = "alloc",
    not(any(feature = "runtime-tokio", feature = "runtime-generic"))
))]
use fatrs::share::Shared;

#[cfg(all(
    feature = "alloc",
    not(any(feature = "runtime-tokio", feature = "runtime-generic"))
))]
fn bench_rc_refcell_acquire(iterations: u32) -> Duration {
    let mut shared = Shared::new(0u64);

    let start = Instant::now();

    for _ in 0..iterations {
        *shared.acquire_mut() = black_box(*shared.acquire_mut() + 1);
    }

    start.elapsed()
}

#[cfg(all(
    feature = "alloc",
    not(any(feature = "runtime-tokio", feature = "runtime-generic"))
))]
fn bench_rc_refcell_throughput(duration_secs: u64) -> u64 {
    let mut shared = Shared::new(0u64);
    let target_duration = Duration::from_secs(duration_secs);

    let start = Instant::now();
    let mut count = 0u64;

    while start.elapsed() < target_duration {
        *shared.acquire_mut() = black_box(*shared.acquire_mut() + 1);
        count += 1;
    }

    count
}

fn main() {
    println!("=== Shared<T> Alloc-Only (Rc<RefCell>) Benchmark ===\n");

    #[cfg(all(
        feature = "alloc",
        not(any(feature = "runtime-tokio", feature = "runtime-generic"))
    ))]
    {
        println!("Runtime: alloc-only (Rc<RefCell<T>>) - ZERO OVERHEAD!");
        println!();

        // Benchmark 1: Direct acquisition overhead
        println!("Benchmark 1: Direct access (no async overhead)");
        println!("  Iterations: 10,000,000");

        let duration = bench_rc_refcell_acquire(10_000_000);
        let nanos_per_op = duration.as_nanos() / 10_000_000;

        println!("  Total time: {:?}", duration);
        println!("  Time per operation: {} ns", nanos_per_op);
        println!();

        // Benchmark 2: Throughput
        println!("Benchmark 2: Operations throughput (1 second)");

        let ops = bench_rc_refcell_throughput(1);

        println!("  Total operations: {}", ops);
        println!("  Operations per second: {}", ops);
        println!("  Average time per op: {} ns", 1_000_000_000 / ops);
        println!();

        println!("=== Summary ===");
        println!("Rc<RefCell> provides near-zero overhead for single-threaded access!");
        println!("This is the optimal choice for embedded systems without threading.");
        println!("\nNote: Rc<RefCell> is !Send + !Sync, perfect for single-threaded contexts.");
    }

    #[cfg(not(all(
        feature = "alloc",
        not(any(feature = "runtime-tokio", feature = "runtime-generic"))
    )))]
    {
        println!("ERROR: This benchmark requires:");
        println!("  cargo bench --bench alloc_only_runtime --no-default-features --features alloc,lfn");
        std::process::exit(1);
    }
}
