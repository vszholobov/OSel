#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]

//! End-to-end child processes: the host-built Go fixture uses ordinary os/exec.

use bootloader_api::{BootInfo, entry_point};
use core::panic::PanicInfo;
use osel::fs::Errno;
use osel::user::Stop;

entry_point!(main, config = &osel::BOOTLOADER_CONFIG);

fn main(boot_info: &'static mut BootInfo) -> ! {
    osel::init(boot_info);
    test_main();
    osel::hlt_loop();
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    osel::testosterone::test_panic_handler(info)
}

#[test_case]
fn go_os_exec_runs_children_and_reclaims_resources() {
    match osel::program::run(
        "/bin/goexec",
        &[b"/bin/goexec"],
        &[b"PATH=/bin", b"GODEBUG=asyncpreemptoff=1"],
    ) {
        Ok(Stop::Exit(0)) => {}
        Ok(other) => panic!("goexec did not complete: {other:?}"),
        Err(Errno::ENOENT) => osel::serial_println!("goexec skipped: host Go was unavailable"),
        Err(error) => panic!("goexec failed to start: {error}"),
    }
}
