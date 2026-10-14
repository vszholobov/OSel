#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]
extern crate alloc;
use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
entry_point!(main, config = &osel::BOOTLOADER_CONFIG);
fn main(info: &'static mut BootInfo) -> ! {
    osel::avganec::init(info.framebuffer.take());
    osel::init(info);
    osel::net::wait_ready().expect("Realtek DHCP lease");
    let mut status = alloc::string::String::new();
    osel::net::status(&mut status);
    assert!(status.contains("Realtek RTL8139 C+"), "{status}");
    drop(status);
    test_main();
    osel::hlt_loop();
}
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    osel::testosterone::test_panic_handler(info)
}
#[test_case]
fn realtek_network_game_receives_fields_input_and_pong() {
    let address = osel::fs::read_file("/bin/network-test-address").unwrap();
    let result = osel::program::run(
        "/bin/tetris",
        &[
            b"/bin/tetris",
            b"--network",
            b"--server",
            &address,
            b"--create",
            b"--trace",
        ],
        &[b"PATH=/bin", b"TERM=xterm", b"GODEBUG=asyncpreemptoff=1"],
    );
    assert!(
        matches!(result, Ok(osel::user::Stop::Exit(0))),
        "Realtek game failed: {result:?}"
    );
}
