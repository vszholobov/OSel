#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]

// The test runner fails a test that doesn't free what it allocated

extern crate alloc;

use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;

entry_point!(main, config = &osel::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    osel::avganec::init(boot_info.framebuffer.take());
    osel::init(boot_info);
    test_main();
    osel::testosterone::fail("the leak went unnoticed");
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    // an 8 byte box takes a 16 byte block
    osel::testosterone::expect_panic(info, &["the test leaked 16 heap bytes"])
}

#[test_case]
fn leaks_a_box() {
    core::mem::forget(alloc::boxed::Box::new(1u64));
}
