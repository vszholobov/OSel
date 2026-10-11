#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![feature(abi_x86_interrupt)]
#![test_runner(crate::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]

#[cfg(not(test))]
#[allow(unused_imports)]
use crate::testosterone::test_main;
#[cfg(test)]
use crate::testosterone::test_panic_handler;
#[cfg(test)]
use core::panic::PanicInfo;

use bootloader_api::BootloaderConfig;

pub mod avganec;
pub mod cereal;
pub mod qemuno;
pub mod testosterone;
pub mod interrupts;
pub mod gdt;
pub mod keyboard;

/// Bootloader settings shared by the kernel and all test kernels
pub const BOOTLOADER_CONFIG: BootloaderConfig = BootloaderConfig::new_default();

pub fn init() {
    gdt::init();
    interrupts::init_idt();
    interrupts::init_pics();
    x86_64::instructions::interrupts::enable();
}

/// Sleeps until the next interrupt forever
pub fn hlt_loop() -> ! {
    loop {
        x86_64::instructions::hlt();
    }
}

#[cfg(test)]
bootloader_api::entry_point!(test_kernel_main, config = &BOOTLOADER_CONFIG);

/// Entry point for `cargo test`
#[cfg(test)]
fn test_kernel_main(boot_info: &'static mut bootloader_api::BootInfo) -> ! {
    avganec::init(boot_info.framebuffer.take());
    init();
    test_main();
    hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}
