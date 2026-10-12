#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use osel::{print, println};
#[cfg(not(test))]
#[allow(unused_imports)]
use osel::testosterone::test_main;

entry_point!(kernel_main, config = &osel::BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    osel::avganec::init(boot_info.framebuffer.take());
    println!("OSel {} (commit {}) woke up!", osel::VERSION, osel::COMMIT);
    osel::init(boot_info);
    #[cfg(test)]
    test_main();
    print_motd();
    println!("OSel ready to go!");
    // the smoke test in CI waits for this line on the serial port
    osel::serial_println!("OSel ready to go!");
    echo_keyboard();
}

/// Greets with /etc/motd from the ramdisk, if there is one
fn print_motd() {
    if let Ok(motd) = osel::fs::read_file("/etc/motd") {
        print!("{}", alloc::string::String::from_utf8_lossy(&motd));
    }
}

/// Prints typed characters until the machine is turned off
fn echo_keyboard() -> ! {
    use x86_64::instructions::interrupts;
    loop {
        // check the buffer with interrupts off, otherwise a key pressed right
        // after the check would wait for the next interrupt to be noticed
        interrupts::disable();
        match osel::keyboard::pop_char() {
            Some(c) => {
                interrupts::enable();
                print!("{}", c);
            }
            None => interrupts::enable_and_hlt(),
        }
    }
}

#[cfg(not(test))]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    let _ = osel::avganec::panic_screen(info);
    osel::hlt_loop();
}

#[cfg(test)]
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    osel::testosterone::test_panic_handler(info);
}
