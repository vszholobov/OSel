#![no_std]
#![cfg_attr(test, no_main)]
#![feature(custom_test_frameworks)]
#![feature(abi_x86_interrupt)]
#![test_runner(crate::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

#[cfg(not(test))]
#[allow(unused_imports)]
use crate::testosterone::test_main;
#[cfg(test)]
use crate::testosterone::test_panic_handler;
#[cfg(test)]
use core::panic::PanicInfo;

pub use bootloader_api;
use bootloader_api::config::Mapping;
use bootloader_api::{BootInfo, BootloaderConfig};
use x86_64::VirtAddr;

pub mod allocator;
pub mod avganec;
pub mod cereal;
pub mod qemuno;
pub mod testosterone;
pub mod interrupts;
pub mod gdt;
pub mod keyboard;
pub mod memory;

/// Bootloader settings shared by the kernel and all test kernels
pub const BOOTLOADER_CONFIG: BootloaderConfig = {
    let mut config = BootloaderConfig::new_default();
    config.mappings.physical_memory = Some(Mapping::Dynamic);
    // keep everything the bootloader maps in the higher half, see `memory`
    config.mappings.dynamic_range_start = Some(memory::KERNEL_DYNAMIC_START);
    config.mappings.dynamic_range_end = Some(memory::KERNEL_DYNAMIC_END);
    config
};

/// Initializes the CPU tables, memory and interrupts.
/// The framebuffer has to be taken from `boot_info` for `avganec` before.
pub fn init(boot_info: &'static mut BootInfo) {
    gdt::init();
    interrupts::init_idt();

    let boot_info: &'static BootInfo = boot_info;
    let physical_memory_offset = *boot_info
        .physical_memory_offset
        .as_ref()
        .expect("bootloader did not map physical memory");
    unsafe { memory::init(VirtAddr::new(physical_memory_offset), &boot_info.memory_regions) };
    allocator::init_heap().expect("heap initialization failed");

    interrupts::init_pics();
    x86_64::instructions::interrupts::enable();
}

static PANICKING: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Marks the kernel as panicking, `false` if it already was (a panic inside a panic handler)
pub fn enter_panic() -> bool {
    !PANICKING.swap(true, core::sync::atomic::Ordering::SeqCst)
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
    init(boot_info);
    test_main();
    hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    test_panic_handler(info)
}
