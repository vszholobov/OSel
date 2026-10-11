#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]

/// The kernel works without a usable framebuffer (like a machine without GOP or VESA):
/// printing is a no-op, everything else keeps working

extern crate alloc;

use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use osel::{avganec, println};

entry_point!(main, config = &osel::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    avganec::init(None);
    osel::init(boot_info);
    test_main();
    osel::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    osel::testosterone::test_panic_handler(info)
}

#[test_case]
fn println_is_a_no_op() {
    println!("nobody sees this");
}

#[test_case]
fn there_is_no_console() {
    assert!(avganec::with_console(|_| ()).is_none());
    assert_eq!(avganec::read_pixel(0, 0), None);
}

#[test_case]
fn heap_and_interrupts_work() {
    let values: alloc::vec::Vec<u64> = (0..1000).collect();
    assert_eq!(values.iter().sum::<u64>(), 499_500);
    let start = osel::interrupts::ticks();
    while osel::interrupts::ticks() < start + 2 {
        x86_64::instructions::hlt();
    }
}
