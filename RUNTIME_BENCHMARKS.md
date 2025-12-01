# Runtime Performance Benchmarks

This document compares the performance characteristics of different `Shared<T>` runtime configurations in fatrs.

## Overview

The fatrs `Shared<T>` type uses conditional compilation to select optimal synchronization primitives:

| Configuration | Internal Type | Overhead | Send + Sync | Use Case |
|---------------|---------------|----------|-------------|----------|
| `runtime-generic` | `Arc<async_lock::Mutex<T>>` | Low, atomic | ✅ Yes | Portable, works with any executor |
| `runtime-tokio` | `Arc<tokio::sync::Mutex<T>>` | Low, atomic | ✅ Yes | Optimized for tokio runtime |
| `alloc` only | `Rc<RefCell<T>>` | Minimal, refcount | ❌ No | Single-threaded, lowest overhead |
| No features | `T` (direct) | **ZERO!** | Inherits | Pure embedded, no allocation |

## Benchmark Results

### Test Environment
- **Hardware**: Modern x86_64 CPU
- **Rust**: 1.85 (2024 edition)
- **Build**: `--release` (optimized)

### 1. runtime-generic (Arc<async_lock::Mutex<T>>)

**Command:**
```bash
cargo bench --bench runtime_comparison
```

**Results:**
```
Runtime: generic (Arc<async_lock::Mutex<T>>)

Benchmark 1: Single-threaded lock acquisition
  Iterations: 1,000,000
  Total time: 36.56ms
  Time per operation: 36 ns ⚡

Benchmark 2: Multi-threaded contention (4 tasks)
  Iterations per task: 100,000
  Total time: 15.11ms
  Time per operation: 37 ns ⚡

Benchmark 3: Operations throughput
  Operations per second: 12,807,370 ops/s
  Average time per op: 78 ns
```

**Analysis:**
- ✅ **Excellent single-threaded performance** (36 ns/op)
- ✅ **Great under contention** (37 ns/op with 4 threads)
- ✅ **Portable** - works with tokio, embassy, smol, any executor
- ✅ **Default choice** for most applications

### 2. runtime-tokio (Arc<tokio::sync::Mutex<T>>)

**Command:**
```bash
cargo bench --bench runtime_comparison --no-default-features \
  --features std,alloc,lfn,runtime-tokio
```

**Results:**
```
Runtime: tokio (Arc<tokio::sync::Mutex<T>>)

Benchmark 1: Single-threaded lock acquisition
  Iterations: 1,000,000
  Total time: 43.02ms
  Time per operation: 43 ns

Benchmark 2: Multi-threaded contention (4 tasks)
  Iterations per task: 100,000
  Total time: 108.85ms
  Time per operation: 272 ns ⚠️

Benchmark 3: Operations throughput
  Operations per second: 11,407,160 ops/s
  Average time per op: 87 ns
```

**Analysis:**
- ⚠️ **Slightly slower single-threaded** (43 ns vs 36 ns)
- ⚠️ **Higher contention overhead** (272 ns vs 37 ns with 4 threads)
- ✅ **Tokio-specific optimizations** for tokio-native code
- 💡 **Use when**: Your entire stack is tokio-based

### 3. alloc-only (Rc<RefCell<T>>)

**Theoretical Performance** (single-threaded only):
```
Runtime: alloc-only (Rc<RefCell<T>>)

Estimated single-threaded performance:
  Time per operation: ~5-10 ns ⚡⚡

Note: Rc<RefCell> is !Send + !Sync
  - Cannot be used across threads
  - Perfect for embedded single-core systems
  - Near-zero overhead (just refcount, no atomics)
```

**Analysis:**
- ⚡⚡ **Lowest overhead** - no atomic operations
- ❌ **!Send + !Sync** - single-threaded only
- ✅ **Perfect for** embedded single-core systems
- 💡 **Use when**: Single-threaded embassy/RTIC applications

### 4. No features (Direct `T`)

**Theoretical Performance**:
```
Runtime: none (Direct T ownership)

Estimated performance:
  Time per operation: ~0-2 ns ⚡⚡⚡

This is pure zero-overhead!
  - No indirection, no refcount, no atomics
  - Just direct field access
  - Compile-time overhead only
```

**Analysis:**
- 🔥 **TRUE ZERO OVERHEAD** - literally just `T`
- ✅ **Compile-time dispatch** via conditional compilation
- ✅ **Perfect for** constrained embedded systems
- 💡 **Use when**: No allocation available, pure embedded

## Performance Comparison Summary

### Single-Threaded Performance
```
Direct T:          ~2 ns    ████████████████████ (10x faster)
Rc<RefCell>:      ~8 ns     ██████████████████ (4.5x faster)
async-lock:       36 ns     ████ (baseline)
tokio:            43 ns     ███ (1.2x slower)
```

### Multi-Threaded Contention (4 tasks)
```
async-lock:       37 ns     ████████████████████ (baseline)
tokio:           272 ns     ██ (7.4x slower)
Rc<RefCell>:     N/A        (not thread-safe)
Direct T:        N/A        (not shareable)
```

### Throughput (ops/second)
```
async-lock:   12.8M ops/s   ████████████████████
tokio:        11.4M ops/s   █████████████████
Rc<RefCell>:  ~125M ops/s   ████████████████████████ (estimated)
Direct T:     ~500M ops/s   ████████████████████████████ (estimated)
```

## Recommendations

### Choose `runtime-generic` (default) when:
- ✅ You want **portability** across executors
- ✅ You need **good all-around performance**
- ✅ You don't know which executor users will choose
- ✅ **Best default choice** for libraries

### Choose `runtime-tokio` when:
- ✅ Your entire stack is **tokio-based**
- ✅ You're building a **tokio-specific** application
- ⚠️ Be aware of higher contention overhead

### Choose `alloc` only (no runtime) when:
- ✅ **Single-threaded** embedded system
- ✅ Using **embassy** or **RTIC** (single-core)
- ✅ Want **minimal overhead** (no atomics)
- ❌ Don't need cross-thread sharing

### Choose no features (direct `T`) when:
- ✅ **Pure embedded** with no allocation
- ✅ **Bare metal** microcontrollers
- ✅ Want **absolute zero overhead**
- ❌ Don't need runtime sharing at all

## Running the Benchmarks Yourself

### Runtime-generic (async-lock):
```bash
cargo bench --bench runtime_comparison
```

### Runtime-tokio:
```bash
cargo bench --bench runtime_comparison --no-default-features \
  --features std,alloc,lfn,runtime-tokio
```

### Compare both:
```bash
# Run both and compare
cargo bench --bench runtime_comparison > generic.txt
cargo bench --bench runtime_comparison --no-default-features \
  --features std,alloc,lfn,runtime-tokio > tokio.txt
diff generic.txt tokio.txt
```

## Conclusion

The `Shared<T>` abstraction provides:

1. **Zero-cost when possible** - Direct `T` for pure embedded
2. **Minimal overhead** - `Rc<RefCell>` for single-threaded
3. **Excellent performance** - `Arc<async_lock::Mutex>` for multi-threaded
4. **Runtime flexibility** - Choose based on your needs

All through **conditional compilation** - pay only for what you use!

This is **idiomatic Rust**: zero-cost abstractions that compile to optimal code for each use case.
