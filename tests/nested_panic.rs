#![no_std]
#![no_main]

/// A panic whose message panics while being shown must not hang or recurse forever

use bootloader_api::{BootInfo, entry_point};
use core::fmt;
use core::panic::PanicInfo;
use core::sync::atomic::{AtomicUsize, Ordering};
use osel::testosterone::fail;

entry_point!(main, config = &osel::BOOTLOADER_CONFIG);

struct Broken;

impl fmt::Display for Broken {
    fn fmt(&self, _: &mut fmt::Formatter) -> fmt::Result {
        panic!("inner panic while formatting");
    }
}

fn main(boot_info: &'static mut BootInfo) -> ! {
    osel::serial_print!("nested_panic::terminates...\t");
    osel::avganec::init(boot_info.framebuffer.take());
    osel::init(boot_info);
    panic!("outer: {}", Broken);
}

static ENTRIES: AtomicUsize = AtomicUsize::new(0);

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    match ENTRIES.fetch_add(1, Ordering::SeqCst) {
        // formatting the outer message inside the panic screen panics
        0 => {
            osel::avganec::panic_screen(info);
            fail("formatting the broken message did not panic");
        }
        // the panic screen must notice the nesting and return without drawing
        1 => {
            if osel::avganec::panic_screen(info) {
                fail("the nested panic was drawn like a first one");
            }
            // the outer panic still holds the screen lock, only the raw port is safe
            osel::cereal::emergency_print(format_args!("[ok]\n"));
            osel::qemuno::exit_qemu(osel::qemuno::QemuExitCode::Success);
        }
        _ => fail("panic handler entered a third time"),
    }
}
