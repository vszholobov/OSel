#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]
use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
entry_point!(main, config = &osel::BOOTLOADER_CONFIG);
fn main(info: &'static mut BootInfo) -> ! {
    osel::avganec::init(info.framebuffer.take());
    osel::init(info);
    osel::net::wait_ready().expect("DHCP lease");
    test_main();
    osel::hlt_loop();
}
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    osel::testosterone::test_panic_handler(info)
}
#[test_case]
fn network_game_receives_fields_handles_input_and_exits() {
    let address = osel::fs::read_file("/bin/network-test-address").unwrap();
    let args: &[&[u8]] = &[
        b"/bin/tetris",
        b"--network",
        b"--server",
        &address,
        b"--create",
        b"--trace",
    ];
    let result = osel::program::run(
        "/bin/tetris",
        args,
        &[b"PATH=/bin", b"TERM=xterm", b"GODEBUG=asyncpreemptoff=1"],
    );
    assert!(
        matches!(result, Ok(osel::user::Stop::Exit(0))),
        "network game failed: {result:?}"
    );
}
