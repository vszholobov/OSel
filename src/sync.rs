//! sync module provides the lock every kernel global uses
//!
//! `IrqSpinlock` disables interrupts while it is held and restores the previous state
//! afterwards, so an interrupt handler can never spin on a lock its own CPU holds.
//! The underlying `osel_core::sync::Spinlock` turns any remaining deadlock (say, a CPU
//! exception handler taking a held lock) into a panic naming both places.

use core::mem::ManuallyDrop;
use core::ops::{Deref, DerefMut};
use osel_core::sync::{Spinlock, SpinlockGuard};
use x86_64::instructions::interrupts;

pub struct IrqSpinlock<T> {
    inner: Spinlock<T>,
}

impl<T> IrqSpinlock<T> {
    pub const fn new(name: &'static str, data: T) -> Self {
        IrqSpinlock { inner: Spinlock::new(name, data) }
    }

    #[track_caller]
    pub fn lock(&self) -> IrqSpinlockGuard<'_, T> {
        let interrupts_were_enabled = interrupts::are_enabled();
        interrupts::disable();
        let guard = self.inner.lock();
        IrqSpinlockGuard { guard: ManuallyDrop::new(guard), interrupts_were_enabled }
    }

    /// Takes the lock if it is free, the interrupt flag is untouched otherwise
    #[track_caller]
    pub fn try_lock(&self) -> Option<IrqSpinlockGuard<'_, T>> {
        let interrupts_were_enabled = interrupts::are_enabled();
        interrupts::disable();
        match self.inner.try_lock() {
            Some(guard) => Some(IrqSpinlockGuard { guard: ManuallyDrop::new(guard), interrupts_were_enabled }),
            None => {
                if interrupts_were_enabled {
                    interrupts::enable();
                }
                None
            }
        }
    }

    pub fn is_locked(&self) -> bool {
        self.inner.is_locked()
    }

    /// Releases the lock no matter who holds it
    ///
    /// # Safety
    /// Only for a panic path that will never return to the holder.
    pub unsafe fn force_unlock(&self) {
        unsafe { self.inner.force_unlock() };
    }
}

pub struct IrqSpinlockGuard<'a, T> {
    guard: ManuallyDrop<SpinlockGuard<'a, T>>,
    interrupts_were_enabled: bool,
}

impl<T> Deref for IrqSpinlockGuard<'_, T> {
    type Target = T;

    fn deref(&self) -> &T {
        &self.guard
    }
}

impl<T> DerefMut for IrqSpinlockGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.guard
    }
}

impl<T> Drop for IrqSpinlockGuard<'_, T> {
    fn drop(&mut self) {
        // unlock first: an interrupt right after `sti` may want the same lock
        unsafe { ManuallyDrop::drop(&mut self.guard) };
        if self.interrupts_were_enabled {
            interrupts::enable();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_lock_disables_and_restores_interrupts() {
        let lock = IrqSpinlock::new("test", 0);
        assert!(interrupts::are_enabled());
        {
            let mut guard = lock.lock();
            *guard += 1;
            assert!(!interrupts::are_enabled());
        }
        assert!(interrupts::are_enabled());
    }

    #[test_case]
    fn test_lock_keeps_interrupts_off_if_they_were_off() {
        let lock = IrqSpinlock::new("test", ());
        interrupts::disable();
        drop(lock.lock());
        assert!(!interrupts::are_enabled());
        interrupts::enable();
    }

    #[test_case]
    fn test_nested_locks_restore_at_the_outermost() {
        let (outer, inner) = (IrqSpinlock::new("outer", ()), IrqSpinlock::new("inner", ()));
        let outer_guard = outer.lock();
        let inner_guard = inner.lock();
        drop(inner_guard);
        assert!(!interrupts::are_enabled(), "the outer lock is still held");
        drop(outer_guard);
        assert!(interrupts::are_enabled());
    }

    #[test_case]
    fn test_failed_try_lock_leaves_interrupts_alone() {
        let lock = IrqSpinlock::new("test", ());
        let guard = lock.lock();
        interrupts::enable();
        assert!(lock.try_lock().is_none());
        assert!(interrupts::are_enabled());
        interrupts::disable();
        assert!(lock.try_lock().is_none());
        assert!(!interrupts::are_enabled());
        // the guard restores the state from when it was taken: enabled
        drop(guard);
        assert!(interrupts::are_enabled());
    }

    #[test_case]
    fn test_timer_keeps_ticking_around_locks() {
        let lock = IrqSpinlock::new("test", 0u64);
        let start = crate::interrupts::ticks();
        while crate::interrupts::ticks() < start + 3 {
            *lock.lock() += 1;
        }
        assert!(*lock.lock() > 0);
    }
}
