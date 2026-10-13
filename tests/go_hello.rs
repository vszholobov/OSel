#![no_std]
#![no_main]
#![feature(custom_test_frameworks)]
#![test_runner(osel::testosterone::test_runner)]
#![reexport_test_harness_main = "test_main"]

//! End-to-end check of stage 2: OSel runs a static Go binary through Linux system calls.
//! The runner builds user-go/gohello with the host `go` and puts it at /bin/gohello; when
//! `go` is not installed the program is absent and this test skips.

extern crate alloc;

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

/// Runs /bin/gohello: it prints, starts goroutines that sleep, and exits with the sum of
/// their results (0+1+4+9+16 = 30). A missing binary means `go` was not on the build
/// machine; the test passes so a Go-less setup is not blocked.
#[test_case]
fn go_binary_runs_goroutines_and_time_sleep() {
    match osel::program::run("/bin/gohello", &[b"/bin/gohello"], &[b"GODEBUG=asyncpreemptoff=1"]) {
        Ok(Stop::Exit(30)) => {}
        Ok(Stop::Exit(code)) => panic!("gohello exited with {code}, expected the goroutine sum 30"),
        Ok(other) => panic!("gohello did not exit cleanly: {other:?}"),
        Err(Errno::ENOENT) => {} // go was not installed when the ramdisk was built: skip
        Err(error) => panic!("gohello failed to start: {error}"),
    }
}
