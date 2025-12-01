//! Runtime-agnostic shared resource abstraction with **zero-overhead** design.
//!
//! This module provides the [`Share`] trait and [`Shared`] wrapper for managing
//! concurrent access to resources across different async runtimes and environments,
//! using conditional compilation to achieve **true zero overhead** in embedded contexts.
//!
//! # Design Philosophy
//!
//! Inspired by the exfat crate's approach, this abstraction allows fatrs to work
//! efficiently across different environments while maintaining zero-cost abstractions:
//!
//! - **Desktop/std**: Thread-safe sharing with `Arc<Mutex<T>>`
//! - **Embedded with alloc**: Single-threaded sharing with `Rc<RefCell<T>>`
//! - **Pure embedded (no alloc)**: Direct ownership `T` - **ZERO overhead!**
//!
//! # Zero-Overhead Guarantee
//!
//! The [`Shared`] type uses conditional compilation to select the optimal representation:
//!
//! ```text
//! ┌─────────────────────┬──────────────────────┬─────────────────────┐
//! │ Feature Combination │ Internal Type        │ Overhead            │
//! ├─────────────────────┼──────────────────────┼─────────────────────┤
//! │ runtime-tokio       │ Arc<tokio::Mutex<T>> │ Thread-safe, atomic │
//! │ runtime-generic     │ Arc<async_lock::...> │ Thread-safe, atomic │
//! │ alloc only          │ Rc<RefCell<T>>       │ Refcount            │
//! │ no features         │ T (direct)           │ ZERO! Just T itself │
//! └─────────────────────┴──────────────────────┴─────────────────────┘
//! ```
//!
//! For pure embedded systems without allocation, `Shared<T>` is **literally just `T`**
//! with zero runtime overhead - no pointers, no refcounts, no indirection!
//!
//! # Runtime Selection
//!
//! Choose your runtime based on your needs:
//!
//! - **`runtime-generic` (default)**: Uses [`async_lock::Mutex`] - portable, works everywhere
//! - **`runtime-tokio`**: Uses [`tokio::sync::Mutex`] - optimized for tokio executor
//! - **`runtime-embassy`**: Future support for embassy-sync primitives
//! - **No runtime + `alloc`**: Uses `Rc<RefCell<T>>` for single-threaded contexts
//! - **No runtime, no `alloc`**: Direct ownership `T` - **pure zero overhead**
//!
//! # Send/Sync Properties (Automatic!)
//!
//! Send/Sync bounds are **automatically determined** by the internal representation:
//!
//! - `Arc<Mutex<T>>` → `Send + Sync` (when `T: Send`)
//! - `Rc<RefCell<T>>` → `!Send + !Sync`
//! - Direct `T` → Inherits `T`'s Send/Sync properties
//!
//! **No manual marker traits needed!** The compiler handles everything.
//!
//! # Examples
//!
//! ```ignore
//! use fatrs::share::{Share, Shared};
//!
//! // Create a shared counter
//! let counter = Shared::new(0u32);
//! let clone = counter.clone();
//!
//! // Acquire and modify
//! {
//!     let mut guard = counter.acquire().await;
//!     *guard += 1;
//! }
//!
//! // Access from clone
//! {
//!     let guard = clone.acquire().await;
//!     assert_eq!(*guard, 1);
//! }
//! ```
//!
//! # Thread Safety
//!
//! The [`Share`] trait itself doesn't require `Send` or `Sync`. However:
//! - `Arc`-based implementations (tokio, async-lock) are `Send + Sync`
//! - `Rc`-based implementations are `!Send + !Sync`
//!
//! Use the `send` feature flag to add `Send` bounds where needed.

#[cfg(feature = "alloc")]
extern crate alloc;

use core::ops::DerefMut;

#[cfg(all(
    feature = "alloc",
    not(any(feature = "runtime-tokio", feature = "runtime-generic"))
))]
use alloc::rc::Rc;

#[cfg(any(feature = "runtime-tokio", feature = "runtime-generic"))]
use alloc::sync::Arc;

#[cfg(all(
    feature = "alloc",
    not(any(feature = "runtime-tokio", feature = "runtime-generic"))
))]
use core::cell::RefCell;

// Import the appropriate mutex based on runtime selection
#[cfg(feature = "runtime-tokio")]
use tokio::sync::Mutex;

#[cfg(all(feature = "runtime-generic", not(feature = "runtime-tokio")))]
use async_lock::Mutex;

/// Trait for types that can be shared and accessed across async contexts.
///
/// This trait abstracts over different sharing mechanisms (Arc, Rc, static refs)
/// and locking strategies (async mutexes, `RefCell`) to provide a unified interface
/// for acquiring mutable access to shared resources.
///
/// # Thread Safety
///
/// Implementations may or may not be `Send`/`Sync` depending on the underlying
/// mechanism. Use appropriate runtime features to get the thread safety you need.
#[allow(async_fn_in_trait)]
pub trait Share: Sized + Clone {
    /// The type being shared.
    type Target;

    /// Acquire mutable access to the shared resource.
    ///
    /// Returns a guard that dereferences to `&mut Self::Target`.
    /// The guard automatically releases access when dropped.
    async fn acquire(&self) -> impl DerefMut<Target = Self::Target>;
}

// Implementation for tokio runtime (Arc<tokio::sync::Mutex<T>>)
#[cfg(feature = "runtime-tokio")]
impl<T> Share for Arc<Mutex<T>> {
    type Target = T;

    #[inline]
    async fn acquire(&self) -> impl DerefMut<Target = T> {
        self.lock().await
    }
}

// Implementation for generic async runtime (Arc<async_lock::Mutex<T>>)
#[cfg(all(feature = "runtime-generic", not(feature = "runtime-tokio")))]
impl<T> Share for Arc<Mutex<T>> {
    type Target = T;

    #[inline]
    async fn acquire(&self) -> impl DerefMut<Target = T> {
        self.lock().await
    }
}

// Implementation for single-threaded no_std (Rc<RefCell<T>>)
#[cfg(all(
    feature = "alloc",
    not(any(feature = "runtime-tokio", feature = "runtime-generic"))
))]
impl<T> Share for Rc<RefCell<T>> {
    type Target = T;

    #[inline]
    async fn acquire(&self) -> impl DerefMut<Target = T> {
        // Wrap in async block to satisfy trait signature
        async { self.borrow_mut() }.await
    }
}

// Implementation for static references (no allocation needed)
impl<T> Share for &'static core::cell::RefCell<T> {
    type Target = T;

    #[inline]
    async fn acquire(&self) -> impl DerefMut<Target = T> {
        // Wrap in async block to satisfy trait signature
        async { self.borrow_mut() }.await
    }
}

/// A convenient wrapper for shared resources that automatically selects
/// the appropriate sharing mechanism based on enabled features.
///
/// # Zero-Overhead Design
///
/// This type uses conditional compilation to select the optimal internal
/// representation based on your feature flags, ensuring **true zero overhead**:
///
/// - **With `runtime-tokio` or `runtime-generic`**: Uses `Arc<Mutex<T>>` for thread-safe sharing
/// - **With `alloc` but no runtime**: Uses `Rc<RefCell<T>>` for single-threaded sharing
/// - **No `alloc` (pure embedded)**: Direct ownership `T` - **ZERO overhead!**
///
/// # Send/Sync Properties
///
/// The Send/Sync properties are **automatic** based on the internal representation:
/// - `Arc<Mutex<T>>`: `Send + Sync` (when `T: Send`)
/// - `Rc<RefCell<T>>`: `!Send + !Sync`
/// - Direct `T`: Inherits `T`'s Send/Sync properties
///
/// # Examples
///
/// ```ignore
/// use fatrs::share::Shared;
///
/// // With runtime features: thread-safe Arc-based sharing
/// #[cfg(feature = "runtime-generic")]
/// {
///     let state = Shared::new(vec![1, 2, 3]);
///     let clone = state.clone();
///     state.acquire().await.push(4);
///     assert_eq!(clone.acquire().await.len(), 4);
/// }
///
/// // Without runtime: direct ownership, zero overhead
/// #[cfg(not(any(feature = "runtime-generic", feature = "runtime-tokio")))]
/// {
///     let mut state = Shared::new(vec![1, 2, 3]);
///     state.acquire_mut().push(4);
///     assert_eq!(state.acquire_mut().len(), 4);
/// }
/// ```
pub struct Shared<T> {
    #[cfg(any(feature = "runtime-tokio", feature = "runtime-generic"))]
    inner: Arc<Mutex<T>>,

    #[cfg(all(
        feature = "alloc",
        not(any(feature = "runtime-tokio", feature = "runtime-generic"))
    ))]
    inner: Rc<RefCell<T>>,

    #[cfg(all(
        not(feature = "alloc"),
        not(any(feature = "runtime-tokio", feature = "runtime-generic"))
    ))]
    inner: T,
}

// Clone implementation for Arc/Rc variants
#[cfg(any(
    feature = "runtime-tokio",
    feature = "runtime-generic",
    all(
        feature = "alloc",
        not(any(feature = "runtime-tokio", feature = "runtime-generic"))
    )
))]
impl<T> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

// For direct ownership variant, Clone requires T: Clone
#[cfg(all(
    not(feature = "alloc"),
    not(any(feature = "runtime-tokio", feature = "runtime-generic"))
))]
impl<T: Clone> Clone for Shared<T> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

impl<T> Shared<T> {
    /// Create a new shared resource.
    ///
    /// The concrete implementation depends on enabled feature flags:
    /// - `runtime-tokio`: `Arc<tokio::sync::Mutex<T>>` - thread-safe
    /// - `runtime-generic`: `Arc<async_lock::Mutex<T>>` - thread-safe
    /// - `alloc` only: `Rc<RefCell<T>>` - single-threaded
    /// - No features: Direct ownership `T` - **zero overhead!**
    #[inline]
    pub fn new(value: T) -> Self {
        Self {
            #[cfg(any(feature = "runtime-tokio", feature = "runtime-generic"))]
            inner: Arc::new(Mutex::new(value)),

            #[cfg(all(
                feature = "alloc",
                not(any(feature = "runtime-tokio", feature = "runtime-generic"))
            ))]
            inner: Rc::new(RefCell::new(value)),

            #[cfg(all(
                not(feature = "alloc"),
                not(any(feature = "runtime-tokio", feature = "runtime-generic"))
            ))]
            inner: value,
        }
    }

    /// Acquire mutable access to the shared resource (async, for Arc/Rc variants).
    ///
    /// Returns a guard that provides `&mut T` access.
    /// The guard automatically releases the lock when dropped.
    ///
    /// # Note
    ///
    /// This method is only available when using `runtime-tokio`, `runtime-generic`,
    /// or `alloc` features. For pure `no_std` without alloc, use `acquire_mut()` instead.
    #[cfg(any(
        feature = "runtime-tokio",
        feature = "runtime-generic",
        all(
            feature = "alloc",
            not(any(feature = "runtime-tokio", feature = "runtime-generic"))
        )
    ))]
    #[inline]
    pub async fn acquire(&self) -> impl DerefMut<Target = T> {
        #[cfg(any(feature = "runtime-tokio", feature = "runtime-generic"))]
        {
            self.inner.lock().await
        }
        #[cfg(all(
            feature = "alloc",
            not(any(feature = "runtime-tokio", feature = "runtime-generic"))
        ))]
        {
            self.inner.borrow_mut()
        }
    }

    /// Acquire mutable access to the shared resource (direct, for no-alloc variant).
    ///
    /// Returns a mutable reference directly to the inner value.
    /// **Zero overhead** - just a direct borrow!
    ///
    /// # Note
    ///
    /// This method is only available in pure no_std without alloc.
    /// For runtime features, use `acquire()` instead.
    #[cfg(all(
        not(feature = "alloc"),
        not(any(feature = "runtime-tokio", feature = "runtime-generic"))
    ))]
    #[inline]
    pub fn acquire_mut(&mut self) -> &mut T {
        &mut self.inner
    }

    /// Get an immutable reference (no-alloc only).
    ///
    /// **Zero overhead** - just a direct borrow!
    #[cfg(all(
        not(feature = "alloc"),
        not(any(feature = "runtime-tokio", feature = "runtime-generic"))
    ))]
    #[inline]
    pub fn get(&self) -> &T {
        &self.inner
    }

    /// Unwrap the shared resource, consuming self and returning the inner value.
    ///
    /// # Panics
    ///
    /// For Arc/Rc variants, panics if there are other references to the same value.
    #[must_use]
    #[inline]
    pub fn into_inner(self) -> T {
        #[cfg(any(feature = "runtime-tokio", feature = "runtime-generic"))]
        {
            Arc::try_unwrap(self.inner)
                .ok()
                .expect("Cannot unwrap Shared with multiple references")
                .into_inner()
        }
        #[cfg(all(
            feature = "alloc",
            not(any(feature = "runtime-tokio", feature = "runtime-generic"))
        ))]
        {
            Rc::try_unwrap(self.inner)
                .ok()
                .expect("Cannot unwrap Shared with multiple references")
                .into_inner()
        }
        #[cfg(all(
            not(feature = "alloc"),
            not(any(feature = "runtime-tokio", feature = "runtime-generic"))
        ))]
        {
            self.inner
        }
    }

    /// Try to acquire mutable access without blocking.
    ///
    /// Returns `Some(guard)` if the lock is immediately available,
    /// or `None` if it's currently held by another task.
    ///
    /// # Note
    ///
    /// This is only available with tokio runtime.
    #[must_use]
    #[cfg(feature = "runtime-tokio")]
    #[inline]
    pub fn try_acquire(&self) -> Option<impl DerefMut<Target = T>> {
        self.inner.try_lock().ok()
    }

    /// Try to acquire mutable access without blocking.
    ///
    /// Returns `Some(guard)` if the lock is immediately available,
    /// or `None` if it's currently held by another task.
    ///
    /// # Note
    ///
    /// This is only available with generic async runtime (uses `async_lock`).
    #[must_use]
    #[cfg(all(feature = "runtime-generic", not(feature = "runtime-tokio")))]
    #[inline]
    pub fn try_acquire(&self) -> Option<impl DerefMut<Target = T>> {
        self.inner.try_lock()
    }
}

// Implement Share for Shared<T> to make it compatible with generic code
// Only for variants that support async acquire
#[cfg(any(
    feature = "runtime-tokio",
    feature = "runtime-generic",
    all(
        feature = "alloc",
        not(any(feature = "runtime-tokio", feature = "runtime-generic"))
    )
))]
impl<T> Share for Shared<T> {
    type Target = T;

    #[inline]
    async fn acquire(&self) -> impl DerefMut<Target = Self::Target> {
        Shared::acquire(self).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(any(
        feature = "runtime-tokio",
        feature = "runtime-generic",
        feature = "alloc"
    ))]
    #[tokio::test]
    async fn test_shared_basic_usage() {
        let shared = Shared::new(42);
        let clone = shared.clone();

        {
            let mut guard = shared.acquire().await;
            *guard = 100;
        }

        {
            let guard = clone.acquire().await;
            assert_eq!(*guard, 100);
        }
    }

    #[cfg(all(
        not(feature = "alloc"),
        not(any(feature = "runtime-tokio", feature = "runtime-generic"))
    ))]
    #[test]
    fn test_shared_no_alloc_zero_overhead() {
        // This variant has ZERO overhead - just direct ownership!
        let mut shared = Shared::new(42);

        // Direct mutable borrow, no Arc, no Rc, no refcount
        *shared.acquire_mut() = 100;
        assert_eq!(*shared.get(), 100);

        // Clone actually clones the value (requires T: Clone)
        let clone = shared.clone();
        assert_eq!(*clone.get(), 100);

        // Unwrap is zero cost - just moves the value
        let value = shared.into_inner();
        assert_eq!(value, 100);
    }

    #[tokio::test]
    async fn test_share_trait_with_arc_mutex() {
        #[cfg(all(feature = "runtime-generic", not(feature = "runtime-tokio")))]
        type TestMutex = async_lock::Mutex<i32>;

        #[cfg(feature = "runtime-tokio")]
        type TestMutex = tokio::sync::Mutex<i32>;

        let shared = Arc::new(TestMutex::new(0));
        let clone = shared.clone();

        {
            let mut guard = shared.acquire().await;
            *guard += 1;
        }

        {
            let guard = clone.acquire().await;
            assert_eq!(*guard, 1);
        }
    }

    #[cfg(any(feature = "runtime-tokio", feature = "runtime-generic"))]
    #[tokio::test]
    async fn test_try_acquire() {
        let shared = Shared::new(vec![1, 2, 3]);

        let _guard = shared.acquire().await;

        // Should fail to acquire while guard is held
        #[cfg(feature = "runtime-generic")]
        assert!(shared.try_acquire().is_none());

        drop(_guard);

        // Should succeed after guard is dropped
        let guard = shared.try_acquire();
        #[cfg(feature = "runtime-generic")]
        assert!(guard.is_some());

        #[cfg(feature = "runtime-tokio")]
        {
            let mut g = guard.unwrap();
            assert_eq!(g.len(), 3);
            g.push(4);
        }
    }
}
