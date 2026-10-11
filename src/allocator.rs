//! allocator module provides the kernel heap
//!
//! The allocator itself lives in `osel_core::heap`, here it gets its memory and
//! becomes the global allocator.

use crate::memory::{self, HEAP_SIZE, HEAP_START};
use core::alloc::{GlobalAlloc, Layout};
use osel_core::heap::FixedSizeBlockAllocator;
use spin::Mutex;
use x86_64::VirtAddr;
use x86_64::instructions::interrupts;
use x86_64::structures::paging::mapper::MapToError;
use x86_64::structures::paging::{PageTableFlags, Size4KiB};

pub use osel_core::heap::BLOCK_SIZES;

pub struct KernelAllocator {
    inner: Mutex<FixedSizeBlockAllocator>,
}

#[global_allocator]
static ALLOCATOR: KernelAllocator = KernelAllocator { inner: Mutex::new(FixedSizeBlockAllocator::new()) };

/// Maps the heap pages and hands them to the allocator
pub fn init_heap() -> Result<(), MapToError<Size4KiB>> {
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
    memory::with_memory(|memory| {
        memory::map_range(&mut memory.mapper, &mut memory.frame_allocator, VirtAddr::new(HEAP_START), HEAP_SIZE, flags)
    })?;
    // the pages were never mapped before, so there are no stale TLB entries to flush
    with_allocator(|allocator| unsafe { allocator.init(HEAP_START as *mut u8, HEAP_SIZE as usize) });
    Ok(())
}

/// Runs `f` with the heap allocator locked
pub fn with_allocator<R>(f: impl FnOnce(&mut FixedSizeBlockAllocator) -> R) -> R {
    // an interrupt handler allocating while we hold the lock would deadlock
    interrupts::without_interrupts(|| f(&mut ALLOCATOR.inner.lock()))
}

unsafe impl GlobalAlloc for KernelAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        with_allocator(|allocator| allocator.allocate(layout))
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        with_allocator(|allocator| unsafe { allocator.deallocate(ptr, layout) })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::alloc::{alloc, dealloc};
    use alloc::boxed::Box;
    use alloc::vec::Vec;

    #[test_case]
    fn test_heap_is_initialized() {
        assert_eq!(with_allocator(|a| a.size()), HEAP_SIZE as usize);
    }

    #[test_case]
    fn test_global_allocator_alignment() {
        for (size, align) in [(1, 1), (24, 64), (2048, 2048), (1, 4096), (5000, 8)] {
            let layout = Layout::from_size_align(size, align).unwrap();
            let ptr = unsafe { alloc(layout) };
            assert!(!ptr.is_null(), "{} {}", size, align);
            assert_eq!(ptr as usize % align, 0, "{} {}", size, align);
            let address = ptr as u64;
            assert!(address >= HEAP_START && address + size as u64 <= HEAP_START + HEAP_SIZE);
            unsafe { dealloc(ptr, layout) };
        }
    }

    #[test_case]
    fn test_out_of_memory_returns_null() {
        let layout = Layout::from_size_align(2 * HEAP_SIZE as usize, 8).unwrap();
        assert!(unsafe { alloc(layout) }.is_null());
    }

    #[test_case]
    fn test_allocation_with_interrupts_running() {
        // the timer keeps interrupting while the heap is busy
        let start = crate::interrupts::ticks();
        while crate::interrupts::ticks() < start + 3 {
            let values: Vec<Box<u64>> = (0..100).map(Box::new).collect();
            assert_eq!(values.iter().map(|b| **b).sum::<u64>(), 4950);
        }
    }
}
