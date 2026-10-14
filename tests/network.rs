#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]
use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
include!("support/network_diagnostics.rs");
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
fn real_http_websocket_tetris_and_cleanup() {
    for _ in 0..2 {
        let result = osel::program::run(
            "/bin/netcheck",
            &[b"/bin/netcheck"],
            &[b"GODEBUG=asyncpreemptoff=1"],
        );
        assert!(
            matches!(result, Ok(osel::user::Stop::Exit(0))),
            "network test failed: {result:?}"
        );
        // Give the exited process's threads a chance to be reaped before leak accounting.
        osel::scheduler::yield_now();
    }
}
