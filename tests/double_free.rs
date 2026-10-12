#![no_std]
#![no_main]

use x86_64::structures::paging::{FrameAllocator, FrameDeallocator};

osel::panic_test!("double_free::is_caught", ["is not handed out: double free or a foreign frame"], {
    osel::memory::with_memory(|m| {
        let frame = m.frame_allocator.allocate_frame().expect("out of frames");
        unsafe {
            m.frame_allocator.deallocate_frame(frame);
            m.frame_allocator.deallocate_frame(frame);
        }
    });
});
