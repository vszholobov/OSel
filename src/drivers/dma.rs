//! Physically contiguous, zeroed DMA pages shared by network drivers.
use crate::memory;
use x86_64::structures::paging::{FrameAllocator, FrameDeallocator, PhysFrame};

pub(super) struct Dma {
    frame: PhysFrame,
    pub ptr: *mut u8,
}
unsafe impl Send for Dma {}
impl Dma {
    pub fn new() -> Option<Self> {
        let (frame, offset) =
            memory::with_memory(|m| (m.frame_allocator.allocate_frame(), m.mapper.phys_offset()));
        let frame = frame?;
        let ptr = (offset + frame.start_address().as_u64()).as_mut_ptr::<u8>();
        unsafe {
            core::ptr::write_bytes(ptr, 0, 4096);
        }
        Some(Self { frame, ptr })
    }
    pub fn physical(&self) -> u64 {
        self.frame.start_address().as_u64()
    }
}
impl Drop for Dma {
    fn drop(&mut self) {
        memory::with_memory(|m| unsafe { m.frame_allocator.deallocate_frame(self.frame) });
    }
}
