//! allocator module provides the kernel heap
//!
//! Small allocations are served from per-size free lists of fixed-size blocks,
//! everything bigger than the largest block goes to a linked list allocator.

use crate::memory::{self, KERNEL_DYNAMIC_END};
use core::alloc::{GlobalAlloc, Layout};
use core::mem;
use core::ptr::{self, NonNull};
use spin::Mutex;
use x86_64::VirtAddr;
use x86_64::structures::paging::mapper::MapToError;
use x86_64::structures::paging::{FrameAllocator, Mapper, Page, PageTableFlags, Size4KiB};

/// Right after the area reserved for the bootloader mappings
pub const HEAP_START: u64 = KERNEL_DYNAMIC_END + 0x1000;
pub const HEAP_SIZE: u64 = 1024 * 1024;

#[global_allocator]
static ALLOCATOR: Locked<FixedSizeBlockAllocator> = Locked::new(FixedSizeBlockAllocator::new());

/// Maps the heap pages and hands them to the allocator
pub fn init_heap() -> Result<(), MapToError<Size4KiB>> {
    let first_page = Page::<Size4KiB>::containing_address(VirtAddr::new(HEAP_START));
    let last_page = Page::<Size4KiB>::containing_address(VirtAddr::new(HEAP_START + HEAP_SIZE - 1));
    let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;

    memory::with_memory(|memory| -> Result<(), MapToError<Size4KiB>> {
        for page in Page::range_inclusive(first_page, last_page) {
            let frame = memory.frame_allocator.allocate_frame().ok_or(MapToError::FrameAllocationFailed)?;
            unsafe { memory.mapper.map_to(page, frame, flags, &mut memory.frame_allocator)?.flush() };
        }
        Ok(())
    })?;

    unsafe { ALLOCATOR.lock().init(HEAP_START as usize, HEAP_SIZE as usize) };
    Ok(())
}

/// A wrapper around `spin::Mutex` to implement foreign traits on it
pub struct Locked<A> {
    inner: Mutex<A>,
}

impl<A> Locked<A> {
    pub const fn new(inner: A) -> Self {
        Locked { inner: Mutex::new(inner) }
    }

    pub fn lock(&self) -> spin::MutexGuard<'_, A> {
        self.inner.lock()
    }
}

/// Block sizes, each one is also its alignment, so they have to be powers of 2
const BLOCK_SIZES: &[usize] = &[8, 16, 32, 64, 128, 256, 512, 1024, 2048];

struct ListNode {
    next: Option<&'static mut ListNode>,
}

pub struct FixedSizeBlockAllocator {
    list_heads: [Option<&'static mut ListNode>; BLOCK_SIZES.len()],
    fallback_allocator: linked_list_allocator::Heap,
}

impl FixedSizeBlockAllocator {
    pub const fn new() -> Self {
        const EMPTY: Option<&'static mut ListNode> = None;
        FixedSizeBlockAllocator {
            list_heads: [EMPTY; BLOCK_SIZES.len()],
            fallback_allocator: linked_list_allocator::Heap::empty(),
        }
    }

    /// # Safety
    /// The memory range must be mapped, unused and passed only once.
    pub unsafe fn init(&mut self, heap_start: usize, heap_size: usize) {
        unsafe { self.fallback_allocator.init(heap_start as *mut u8, heap_size) };
    }

    fn fallback_alloc(&mut self, layout: Layout) -> *mut u8 {
        match self.fallback_allocator.allocate_first_fit(layout) {
            Ok(ptr) => ptr.as_ptr(),
            Err(()) => ptr::null_mut(),
        }
    }
}

impl Default for FixedSizeBlockAllocator {
    fn default() -> Self {
        Self::new()
    }
}

/// Index of the smallest block that fits `layout`, `None` for big allocations
fn list_index(layout: &Layout) -> Option<usize> {
    let required_block_size = layout.size().max(layout.align());
    BLOCK_SIZES.iter().position(|&s| s >= required_block_size)
}

unsafe impl GlobalAlloc for Locked<FixedSizeBlockAllocator> {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let mut allocator = self.lock();
        match list_index(&layout) {
            Some(index) => match allocator.list_heads[index].take() {
                Some(node) => {
                    allocator.list_heads[index] = node.next.take();
                    node as *mut ListNode as *mut u8
                }
                None => {
                    // no free block of this size yet, carve a new one
                    let block_size = BLOCK_SIZES[index];
                    let layout = Layout::from_size_align(block_size, block_size).unwrap();
                    allocator.fallback_alloc(layout)
                }
            },
            None => allocator.fallback_alloc(layout),
        }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        let mut allocator = self.lock();
        match list_index(&layout) {
            Some(index) => {
                let new_node = ListNode { next: allocator.list_heads[index].take() };
                // every block is at least as big and aligned as a ListNode
                assert!(mem::size_of::<ListNode>() <= BLOCK_SIZES[index]);
                assert!(mem::align_of::<ListNode>() <= BLOCK_SIZES[index]);
                let new_node_ptr = ptr as *mut ListNode;
                unsafe {
                    new_node_ptr.write(new_node);
                    allocator.list_heads[index] = Some(&mut *new_node_ptr);
                }
            }
            None => {
                let ptr = NonNull::new(ptr).unwrap();
                unsafe { allocator.fallback_allocator.deallocate(ptr, layout) };
            }
        }
    }
}
