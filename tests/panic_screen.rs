#![no_std]
#![no_main]

/// Checks that a CPU exception ends up on the red panic screen with the fault details

extern crate alloc;

use alloc::format;
use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use osel::avganec::{self, Color};
use osel::qemuno::{QemuExitCode, exit_qemu};
use osel::{serial_print, serial_println};

const FAULT_ADDRESS: u64 = 0xdead_beef_000;

entry_point!(main, config = &osel::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    serial_print!("panic_screen::page_fault_is_shown...\t");
    avganec::init(boot_info.framebuffer.take());
    osel::init(boot_info);

    unsafe { core::ptr::read_volatile(FAULT_ADDRESS as *const u64) };

    serial_println!("[failed]\n\nError: execution continued after page fault");
    exit_qemu(QemuExitCode::Failed);
    osel::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    avganec::panic_screen(info);

    let message = format!("{}", info.message());
    let failure = if !message.contains("PAGE FAULT") {
        Some("no PAGE FAULT in the panic message")
    } else if !message.contains(&format!("{:#x}", FAULT_ADDRESS)) {
        Some("no fault address in the panic message")
    } else if avganec::read_pixel(0, 0) != Some(Color::RED) {
        Some("screen is not red")
    } else {
        None
    };

    match failure {
        None => {
            serial_println!("[ok]");
            exit_qemu(QemuExitCode::Success);
        }
        Some(reason) => {
            serial_println!("[failed]\n\nError: {}", reason);
            exit_qemu(QemuExitCode::Failed);
        }
    }
    osel::hlt_loop();
}
