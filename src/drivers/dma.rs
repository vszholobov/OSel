//! Physically contiguous, zeroed DMA pages shared by network drivers.
use crate::memory;
use alloc::vec::Vec;
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
        unsafe { core::ptr::write_bytes(self.ptr, 0, 4096) };
        memory::with_memory(|m| unsafe { m.frame_allocator.deallocate_frame(self.frame) });
    }
}

/// A bounded contiguous region for firmware-owned host memory, below 4 GiB.
/// Allocate outside NETWORK; no target may use it before initialization finishes.
pub(super) struct DmaRegion {
    frames: Vec<PhysFrame>,
    pub ptr: *mut u8,
    pub length: usize,
}
unsafe impl Send for DmaRegion {}
impl DmaRegion {
    pub fn new(length: usize) -> Option<Self> {
        if length == 0 || length > 1024 * 1024 {
            return None;
        }
        let pages = length.div_ceil(4096);
        // Exhaust a bounded prefix of the LIFO cache while retaining candidates, then
        // look for a fresh consecutive run. Holding candidates prevents picking the
        // same fragmented cached pages forever. Vec capacity is allocated before MEMORY.
        let mut frames = Vec::with_capacity(pages + 4096);
        let (start, offset) = memory::with_memory(|m| {
            let mut run = 0;
            let mut previous = u64::MAX;
            while frames.len() < frames.capacity() {
                let Some(frame) = m.frame_allocator.allocate_frame() else {
                    break;
                };
                let physical = frame.start_address().as_u64();
                frames.push(frame);
                if physical >= 0x1_0000_0000 {
                    run = 0;
                    previous = u64::MAX;
                    continue;
                }
                run = if previous.checked_add(4096) == Some(physical) {
                    run + 1
                } else {
                    1
                };
                previous = physical;
                if run == pages {
                    let first = frames.len() - pages;
                    for frame in frames.drain(..first) {
                        unsafe { m.frame_allocator.deallocate_frame(frame) };
                    }
                    return (
                        Some(frames[0].start_address().as_u64()),
                        m.mapper.phys_offset(),
                    );
                }
            }
            for frame in frames.drain(..) {
                unsafe { m.frame_allocator.deallocate_frame(frame) };
            }
            (None, m.mapper.phys_offset())
        });
        let ptr = (offset + start?).as_mut_ptr::<u8>();
        frames.shrink_to_fit();
        unsafe { core::ptr::write_bytes(ptr, 0, pages * 4096) };
        Some(Self {
            frames,
            ptr,
            length,
        })
    }
    pub fn physical(&self) -> u64 {
        self.frames[0].start_address().as_u64()
    }
}
impl Drop for DmaRegion {
    fn drop(&mut self) {
        // Callers stop firmware before dropping its shared memory.
        unsafe { core::ptr::write_bytes(self.ptr, 0, self.frames.len() * 4096) };
        memory::with_memory(|m| {
            for frame in &self.frames {
                unsafe { m.frame_allocator.deallocate_frame(*frame) };
            }
        });
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use osel_core::frames::FrameSource;
    #[test_case]
    fn contiguous_firmware_memory_is_zeroed_and_returns_every_frame() {
        let before = memory::with_memory(|m| m.frame_allocator.used_frames());
        assert!(DmaRegion::new(0).is_none());
        assert!(DmaRegion::new(1024 * 1024 + 1).is_none());
        for _ in 0..3 {
            let region = DmaRegion::new(7 * 4096 + 3).unwrap();
            assert_eq!(region.length, 7 * 4096 + 3);
            assert!(region.physical() + 8 * 4096 <= 0x1_0000_0000);
            for (i, frame) in region.frames.iter().enumerate() {
                assert_eq!(
                    frame.start_address().as_u64(),
                    region.physical() + i as u64 * 4096
                );
            }
            let bytes = unsafe { core::slice::from_raw_parts(region.ptr, 8 * 4096) };
            assert!(bytes.iter().all(|&b| b == 0));
            unsafe { region.ptr.add(7 * 4096).write_volatile(0x5a) };
            drop(region);
            assert_eq!(
                memory::with_memory(|m| m.frame_allocator.used_frames()),
                before
            );
        }
    }
}
