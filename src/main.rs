#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]

extern crate alloc;

use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
#[cfg(not(test))]
#[allow(unused_imports)]
use osel::testosterone::test_main;
use osel::{print, println};

entry_point!(kernel_main, config = &osel::BOOTLOADER_CONFIG);

fn kernel_main(boot_info: &'static mut BootInfo) -> ! {
    osel::avganec::init(boot_info.framebuffer.take());
    println!("OSel {} (commit {}) woke up!", osel::VERSION, osel::COMMIT);
    osel::init(boot_info);
    #[cfg(test)]
    test_main();
    print_motd();
    run_hello();
    println!("OSel ready to go!");
    // the smoke test in CI waits for this line on the serial port
    osel::serial_println!("OSel ready to go!");
    run_tetris();
    echo_keyboard();
}

/// Greets with /etc/motd from the ramdisk, if there is one
fn print_motd() {
    if let Ok(motd) = osel::fs::read_file("/etc/motd") {
        print!("{}", alloc::string::String::from_utf8_lossy(&motd));
    }
}

/// Runs the first program, /bin/hello from the ramdisk, if it is there
fn run_hello() {
    match osel::program::run("/bin/hello", &[b"/bin/hello"], &[]) {
        Ok(osel::user::Stop::Exit(0)) | Err(osel::fs::Errno::ENOENT) => {}
        Ok(stop) => {
            println!("/bin/hello ended with {:?}", stop);
            osel::serial_println!("/bin/hello ended with {:?}", stop);
        }
        Err(error) => {
            println!("/bin/hello did not start: {}", error);
            osel::serial_println!("/bin/hello did not start: {}", error);
        }
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

/// Runs the pinned Go game; clear remains an ordinary child program found through PATH.
fn run_tetris() {
    match osel::program::run(
        "/bin/tetris",
        &[b"/bin/tetris", b"--single"],
        &[b"PATH=/bin", b"TERM=xterm", b"GODEBUG=asyncpreemptoff=1"],
    ) {
        Ok(osel::user::Stop::Exit(0)) | Err(osel::fs::Errno::ENOENT) => {}
        result => {
            println!("/bin/tetris ended: {:?}", result);
            osel::serial_println!("/bin/tetris ended: {:?}", result);
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
