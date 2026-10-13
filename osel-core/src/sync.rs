//! Spinlock that reports deadlocks instead of hanging
//!
//! A kernel lock is taken with interrupts disabled, so on a single CPU waiting for it
//! can never end if the holder is the waiter itself (say, a breakpoint handler printing
//! while the console is locked). After `max_spins` attempts the lock panics and names
//! itself, the place that holds it and the place that waits.

use core::cell::UnsafeCell;
use core::fmt;
use core::ops::{Deref, DerefMut};
use core::panic::Location;
use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicPtr, Ordering};

/// Attempts before a waiting `lock` declares a deadlock
pub const DEFAULT_MAX_SPINS: usize = 50_000_000;

pub struct Spinlock<T> {
    name: &'static str,
    locked: AtomicBool,
    /// Where the current holder took the lock, for the deadlock report
    holder: AtomicPtr<Location<'static>>,
    max_spins: usize,
    data: UnsafeCell<T>,
}

// the lock hands out access to the data to one holder at a time
unsafe impl<T: Send> Sync for Spinlock<T> {}
unsafe impl<T: Send> Send for Spinlock<T> {}

impl<T> Spinlock<T> {
    pub const fn new(name: &'static str, data: T) -> Self {
        Self::with_max_spins(name, data, DEFAULT_MAX_SPINS)
    }

    pub const fn with_max_spins(name: &'static str, data: T, max_spins: usize) -> Self {
        Spinlock {
            name,
            locked: AtomicBool::new(false),
            holder: AtomicPtr::new(ptr::null_mut()),
            max_spins,
            data: UnsafeCell::new(data),
        }
    }

    pub fn name(&self) -> &'static str {
        self.name
    }

    /// The value, consuming the lock
    pub fn into_inner(self) -> T {
        self.data.into_inner()
    }

    /// Waits for the lock, panics with a deadlock report after `max_spins` attempts
    #[track_caller]
    pub fn lock(&self) -> SpinlockGuard<'_, T> {
        let waiter = Location::caller();
        for _ in 0..self.max_spins {
            if let Some(guard) = self.try_lock_at(waiter) {
                return guard;
            }
            core::hint::spin_loop();
        }
        let holder = self.holder.load(Ordering::Relaxed);
        // SAFETY: only ever set from `Location::caller()`, which is 'static
        let holder = unsafe { holder.as_ref() };
        panic!(
            "deadlock on lock `{}`: held since {}, waiting at {}",
            self.name,
            holder.map_or(&"<unknown>" as &dyn fmt::Display, |l| l as &dyn fmt::Display),
            waiter
        );
    }

    /// Takes the lock if it is free
    #[track_caller]
    pub fn try_lock(&self) -> Option<SpinlockGuard<'_, T>> {
        self.try_lock_at(Location::caller())
    }

    fn try_lock_at(&self, location: &'static Location<'static>) -> Option<SpinlockGuard<'_, T>> {
        self.locked.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed).ok()?;
        self.holder.store(location as *const Location as *mut Location, Ordering::Relaxed);
        Some(SpinlockGuard { lock: self })
    }

    pub fn is_locked(&self) -> bool {
        self.locked.load(Ordering::Relaxed)
    }

    /// Releases the lock no matter who holds it
    ///
    /// # Safety
    /// Only for a panic path that will never return to the holder.
    pub unsafe fn force_unlock(&self) {
        self.holder.store(ptr::null_mut(), Ordering::Relaxed);
        self.locked.store(false, Ordering::Release);
    }
}

pub struct SpinlockGuard<'a, T> {
    lock: &'a Spinlock<T>,
}

impl<T> Deref for SpinlockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        // SAFETY: the guard proves the lock is held
        unsafe { &*self.lock.data.get() }
    }
}

impl<T> DerefMut for SpinlockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        // SAFETY: the guard proves the lock is held, and it is borrowed mutably
        unsafe { &mut *self.lock.data.get() }
    }
}

impl<T> Drop for SpinlockGuard<'_, T> {
    fn drop(&mut self) {
        self.lock.holder.store(ptr::null_mut(), Ordering::Relaxed);
        self.lock.locked.store(false, Ordering::Release);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use alloc::string::String;
    use std::sync::Arc;

    #[test]
    fn lock_gives_exclusive_access() {
        let lock = Spinlock::new("counter", 0);
        *lock.lock() += 1;
        *lock.lock() += 1;
        assert_eq!(*lock.lock(), 2);
        assert_eq!(lock.name(), "counter");
    }

    #[test]
    fn try_lock_fails_while_locked() {
        let lock = Spinlock::new("lock", ());
        let guard = lock.lock();
        assert!(lock.is_locked());
        assert!(lock.try_lock().is_none());
        drop(guard);
        assert!(!lock.is_locked());
        assert!(lock.try_lock().is_some());
    }

    #[test]
    fn deadlock_is_reported_with_both_places() {
        let lock = Arc::new(Spinlock::with_max_spins("TEST", 0u8, 1000));
        let cloned = lock.clone();
        let message = std::thread::spawn(move || {
            let _held = cloned.lock();
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _again = cloned.lock();
            }));
            let payload = result.expect_err("locking twice must panic");
            payload.downcast_ref::<String>().cloned().unwrap_or_default()
        })
        .join()
        .unwrap();
        assert!(message.starts_with("deadlock on lock `TEST`: held since "), "{message}");
        // both places are in this file, on different lines
        let file = file!();
        assert_eq!(message.matches(file).count(), 2, "{message}");
        assert!(message.contains(", waiting at "));
    }

    #[test]
    fn force_unlock_releases() {
        let lock = Spinlock::new("lock", 5);
        let guard = lock.lock();
        core::mem::forget(guard);
        assert!(lock.try_lock().is_none());
        unsafe { lock.force_unlock() };
        assert_eq!(*lock.try_lock().unwrap(), 5);
    }

    /// Nikka's spinlock consistency test: an invariant over two fields holds under contention
    #[test]
    fn invariant_holds_under_contention() {
        let lock = Arc::new(Spinlock::new("pair", (0u64, 0u64)));
        let threads: std::vec::Vec<_> = (0..8)
            .map(|_| {
                let lock = lock.clone();
                std::thread::spawn(move || {
                    for _ in 0..10_000 {
                        let mut pair = lock.lock();
                        assert_eq!(pair.0 * 2, pair.1, "torn update seen");
                        pair.0 += 1;
                        pair.1 += 2;
                    }
                })
            })
            .collect();
        for thread in threads {
            thread.join().unwrap();
        }
        assert_eq!(*lock.lock(), (80_000, 160_000));
    }

    #[test]
    fn holder_is_cleared_after_unlock() {
        let lock = Spinlock::with_max_spins("lock", (), 10);
        drop(lock.lock());
        let _held = lock.try_lock().unwrap();
        // a report now names the try_lock place, not the old holder
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| drop(lock.lock())));
        let message = format!("{:?}", result.unwrap_err().downcast_ref::<String>());
        assert!(!message.contains("<unknown>"), "{message}");
    }
}
