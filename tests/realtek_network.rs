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
    osel::net::wait_ready().expect("Realtek DHCP lease");
    let mut status = alloc::string::String::new();
    osel::net::status(&mut status);
    assert!(status.contains("Realtek RTL8139 C+"), "{status}");
    drop(status);
    test_main();
    osel::hlt_loop();
}
extern crate alloc;
#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    osel::testosterone::test_panic_handler(info)
}
#[test_case]
fn realtek_http_websocket_large_transfer_and_cleanup() {
    for _ in 0..2 {
        let result = osel::program::run(
            "/bin/netcheck",
            &[b"/bin/netcheck"],
            &[b"GODEBUG=asyncpreemptoff=1"],
        );
        assert!(
            matches!(result, Ok(osel::user::Stop::Exit(0))),
            "Realtek network failed: {result:?}"
        );
        osel::scheduler::yield_now();
    }
}
