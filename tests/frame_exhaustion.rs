#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]

/// Handing out every physical frame: all unique, in ascending order, then a sticky `None`;
/// freeing them all and handing them out again

extern crate alloc;

use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use alloc::vec::Vec;
use osel::memory::{FrameSource, LOW_MEMORY_END, total_usable_frames, used_frames, with_memory};
use x86_64::structures::paging::{FrameAllocator, FrameDeallocator, PhysFrame};

entry_point!(main, config = &osel::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    osel::avganec::init(boot_info.framebuffer.take());
    osel::init(boot_info);
    test_main();
    osel::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    osel::testosterone::test_panic_handler(info)
}

const GARBAGE: u64 = 0xdead_beef_dead_beef;

/// Nikka 2-mm-1 `allocated_frames_are_not_used`: every handed out frame is filled with
/// garbage, if any of them was in use the kernel breaks right away
#[test_case]
fn every_frame_once() {
    // allocated up front: the heap is not used while `MEMORY` is held
    let mut frames = Vec::with_capacity(total_usable_frames());
    let before = with_memory(|m| {
        let before = m.frame_allocator.used_frames();
        assert_eq!(m.frame_allocator.free_list_len(), 0, "nothing was freed yet");
        let offset = m.mapper.phys_offset();
        // with an empty free list every frame is fresh, and fresh frames come in order
        while let Some(frame) = m.frame_allocator.allocate_frame() {
            let address = frame.start_address().as_u64();
            assert!(frames.last().is_none_or(|last: &PhysFrame| address > last.start_address().as_u64()), "frames are not ascending");
            assert!(address >= LOW_MEMORY_END);
            let words = (offset + address).as_mut_ptr::<u64>();
            for i in 0..512 {
                unsafe { words.add(i).write_volatile(GARBAGE) };
            }
            frames.push(frame);
        }
        assert!(m.frame_allocator.allocate_frame().is_none(), "exhaustion is sticky");
        before
    });
    assert_eq!(frames.len() + before, total_usable_frames(), "frames went missing");
    // the runner gives test kernels 128 MiB
    let mib = frames.len() * 4096 / (1024 * 1024);
    assert!((64..=128).contains(&mib), "{} MiB of free frames", mib);

    // all of them come back, then out again in reverse order
    with_memory(|m| {
        for &frame in &frames {
            unsafe { m.frame_allocator.deallocate_frame(frame) };
        }
        assert_eq!(m.frame_allocator.used_frames(), before);
        for &frame in frames.iter().rev() {
            assert_eq!(m.frame_allocator.allocate_frame(), Some(frame), "freed frames are reused LIFO");
        }
        assert!(m.frame_allocator.allocate_frame().is_none());
        for &frame in &frames {
            unsafe { m.frame_allocator.deallocate_frame(frame) };
        }
    });
    assert_eq!(used_frames(), before, "the used counter is back");
}

#[test_case]
fn kernel_still_works_after_the_garbage() {
    let values: alloc::vec::Vec<u64> = (0..1000).collect();
    assert_eq!(values.iter().sum::<u64>(), 499_500);
    osel::println!("still alive");
    let start = osel::interrupts::ticks();
    while osel::interrupts::ticks() < start + 2 {
        x86_64::instructions::hlt();
    }
}
