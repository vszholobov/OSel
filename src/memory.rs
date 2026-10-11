//! memory module manages page tables and physical frames
//!
//! Virtual address space layout:
//! - lower half (`0x0000_...`) is left free for user programs;
//! - `0xffff_8000_0000_0000..KERNEL_DYNAMIC_END`: everything the bootloader places itself
//!   (kernel image, stack, boot info, framebuffer, physical memory mapping);
//! - `HEAP_START..`: kernel heap, see `allocator`.

use bootloader_api::info::{MemoryRegion, MemoryRegionKind};
use spin::Mutex;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{FrameAllocator, OffsetPageTable, PageTable, PhysFrame, Size4KiB};
use x86_64::{PhysAddr, VirtAddr};

pub const KERNEL_DYNAMIC_START: u64 = 0xffff_8000_0000_0000;
pub const KERNEL_DYNAMIC_END: u64 = 0xffff_bfff_ffff_f000;

pub struct Memory {
    pub mapper: OffsetPageTable<'static>,
    pub frame_allocator: BootInfoFrameAllocator,
}

static MEMORY: Mutex<Option<Memory>> = Mutex::new(None);

/// Takes over the active page table and the usable memory reported by the bootloader
///
/// # Safety
/// The whole physical memory must be mapped at `physical_memory_offset`,
/// and the function must be called only once.
pub unsafe fn init(physical_memory_offset: VirtAddr, memory_regions: &'static [MemoryRegion]) {
    let level_4_table = unsafe { active_level_4_table(physical_memory_offset) };
    let mapper = unsafe { OffsetPageTable::new(level_4_table, physical_memory_offset) };
    let frame_allocator = unsafe { BootInfoFrameAllocator::new(memory_regions) };
    *MEMORY.lock() = Some(Memory { mapper, frame_allocator });
}

/// Runs `f` with exclusive access to the page table and the frame allocator
pub fn with_memory<R>(f: impl FnOnce(&mut Memory) -> R) -> R {
    // page faults or other interrupts must not try to take the lock while we hold it
    x86_64::instructions::interrupts::without_interrupts(|| {
        let mut memory = MEMORY.lock();
        f(memory.as_mut().expect("memory is not initialized"))
    })
}

unsafe fn active_level_4_table(physical_memory_offset: VirtAddr) -> &'static mut PageTable {
    let (level_4_table_frame, _) = Cr3::read();
    let virt = physical_memory_offset + level_4_table_frame.start_address().as_u64();
    unsafe { &mut *virt.as_mut_ptr() }
}

/// Hands out usable physical frames one by one, never reuses them
pub struct BootInfoFrameAllocator {
    memory_regions: &'static [MemoryRegion],
    region: usize,
    next: PhysAddr,
}

impl BootInfoFrameAllocator {
    /// # Safety
    /// Frames of regions marked usable must really be unused.
    pub unsafe fn new(memory_regions: &'static [MemoryRegion]) -> Self {
        BootInfoFrameAllocator {
            memory_regions,
            region: 0,
            // keep the zero frame out: a zero physical address usually means a bug
            next: PhysAddr::new(0x1000),
        }
    }

    /// Number of usable bytes reported by the bootloader
    pub fn usable_memory(&self) -> u64 {
        self.memory_regions
            .iter()
            .filter(|r| r.kind == MemoryRegionKind::Usable)
            .map(|r| r.end - r.start)
            .sum()
    }
}

unsafe impl FrameAllocator<Size4KiB> for BootInfoFrameAllocator {
    fn allocate_frame(&mut self) -> Option<PhysFrame> {
        while let Some(region) = self.memory_regions.get(self.region) {
            let start = PhysAddr::new(region.start).align_up(4096u64).max(self.next);
            if region.kind == MemoryRegionKind::Usable && start + 4096u64 <= PhysAddr::new(region.end) {
                self.next = start + 4096u64;
                return Some(PhysFrame::containing_address(start));
            }
            self.region += 1;
        }
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use x86_64::structures::paging::{Mapper, Page, PageTableFlags, Translate};

    #[test_case]
    fn test_translate_kernel_address() {
        static VALUE: u64 = 42;
        let virt = VirtAddr::from_ptr(&VALUE);
        assert!(virt.as_u64() >= KERNEL_DYNAMIC_START, "kernel is not in the higher half: {:?}", virt);
        let phys = with_memory(|m| m.mapper.translate_addr(virt)).expect("kernel address is not mapped");
        // the same value must be visible through the physical memory mapping
        let through_phys = with_memory(|m| m.mapper.phys_offset() + phys.as_u64());
        assert_eq!(unsafe { *through_phys.as_ptr::<u64>() }, 42);
    }

    #[test_case]
    fn test_map_page() {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(0x1234_5000));
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        let frame = with_memory(|m| {
            let frame = m.frame_allocator.allocate_frame().expect("out of frames");
            unsafe { m.mapper.map_to(page, frame, flags, &mut m.frame_allocator) }
                .expect("map_to failed")
                .flush();
            frame
        });

        let ptr: *mut u64 = page.start_address().as_mut_ptr();
        unsafe { ptr.write_volatile(0xf00d_cafe) };
        let through_phys = with_memory(|m| m.mapper.phys_offset() + frame.start_address().as_u64());
        assert_eq!(unsafe { through_phys.as_ptr::<u64>().read_volatile() }, 0xf00d_cafe);

        with_memory(|m| m.mapper.unmap(page).expect("unmap failed").1.flush());
    }

    #[test_case]
    fn test_frames_are_distinct() {
        let (a, b) = with_memory(|m| {
            (m.frame_allocator.allocate_frame().unwrap(), m.frame_allocator.allocate_frame().unwrap())
        });
        assert_ne!(a, b);
        assert_ne!(a.start_address().as_u64(), 0);
    }
}
