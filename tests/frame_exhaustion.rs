#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]

/// Handing out every physical frame: all unique, in ascending order, then a sticky `None`

use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use osel::memory::{LOW_MEMORY_END, with_memory};
use x86_64::structures::paging::FrameAllocator;

entry_point!(main, config = &osel::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    osel::init(boot_info);
    test_main();
    osel::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    osel::testosterone::test_panic_handler(info)
}

#[test_case]
fn every_frame_once() {
    let (count, before) = with_memory(|m| {
        let before = m.frame_allocator.allocated();
        let mut last = 0;
        let mut count = 0u64;
        while let Some(frame) = m.frame_allocator.allocate_frame() {
            let address = frame.start_address().as_u64();
            assert!(address > last || count == 0, "frames are not ascending");
            assert!(address >= LOW_MEMORY_END);
            last = address;
            count += 1;
        }
        assert!(m.frame_allocator.allocate_frame().is_none(), "exhaustion is sticky");
        (count, before)
    });
    // QEMU gives the machine 128 MiB by default
    let mib = (count + before as u64) * 4096 / (1024 * 1024);
    assert!((64..=128).contains(&mib), "{} MiB of usable frames", mib);
}
