#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]

// The test runner fails a test that keeps a physical frame

use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use x86_64::structures::paging::FrameAllocator;

entry_point!(main, config = &osel::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    osel::avganec::init(boot_info.framebuffer.take());
    osel::init(boot_info);
    test_main();
    osel::testosterone::fail("the leak went unnoticed");
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    osel::testosterone::expect_panic(info, &["the test leaked 1 frames"])
}

#[test_case]
fn leaks_a_frame() {
    osel::memory::with_memory(|m| m.frame_allocator.allocate_frame()).expect("out of frames");
}
