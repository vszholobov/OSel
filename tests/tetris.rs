#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]

//! Real Go game, external clear, serial input, gravity and clean process teardown.
//! Runner waits for trace acknowledgements and checks the whole input scenario.

use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use osel::{fs::Errno, user::Stop};

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

#[test_case]
fn single_player_responds_to_serial_input() {
    match osel::program::run(
        "/bin/tetris",
        &[b"/bin/tetris", b"--single", b"--seed", b"2", b"--trace"],
        &[b"PATH=/bin", b"TERM=xterm", b"GODEBUG=asyncpreemptoff=1"],
    ) {
        Ok(Stop::Exit(0)) => {}
        Err(Errno::ENOENT) => osel::serial_println!("\nTETRIS SKIP: Go is not installed"),
        result => panic!("tetris failed: {:?}", result),
    }
}
