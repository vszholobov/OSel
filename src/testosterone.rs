//! testosterone module provides testing utils

use crate::qemuno::QemuExitCode;
use crate::qemuno::exit_qemu;
use crate::serial_print;
use crate::serial_println;
use core::panic::PanicInfo;

#[allow(dead_code)]
pub trait Testable {
    fn run(&self) -> ();
}

impl<T> Testable for T
where
    T: Fn(),
{
    fn run(&self) {
        serial_print!("{}...\t", core::any::type_name::<T>());
        self();
        serial_println!("[ok]");
    }
}

#[allow(dead_code)]
pub fn test_runner(tests: &[&dyn Testable]) {
    serial_println!("Running {} tests", tests.len());
    for test in tests {
        test.run(); // new
    }
    exit_qemu(QemuExitCode::Success);
}

#[cfg(not(test))]
pub fn test_main() {
    // dummy function to void error in IDE. Not affecting build or execution, just IDE highlights
}

/// Reports the failed test and exits QEMU. Works even if the panic happened while
/// printing to the serial port or inside another panic.
#[allow(dead_code)]
pub fn test_panic_handler(info: &PanicInfo) -> ! {
    x86_64::instructions::interrupts::disable();
    if !crate::enter_panic() {
        crate::cereal::emergency_print(format_args!("[failed]\n\nError: nested panic: {}\n", info));
        exit_qemu(QemuExitCode::Failed);
    }
    unsafe { crate::cereal::SERIAL1.force_unlock() };
    serial_println!("[failed]\n");
    serial_println!("Error: {}\n", info);
    exit_qemu(QemuExitCode::Failed);
}

/// Marks a `harness = false` test as passed and exits QEMU
pub fn pass() -> ! {
    serial_println!("[ok]");
    exit_qemu(QemuExitCode::Success);
}

/// Marks a `harness = false` test as failed and exits QEMU
pub fn fail(reason: &str) -> ! {
    serial_println!("[failed]\n\nError: {}\n", reason);
    exit_qemu(QemuExitCode::Failed);
}

/// Panic handler of tests that expect a panic: shows the panic screen, then passes
/// if the message contains every `expected` part (and the screen turned red)
pub fn expect_panic(info: &PanicInfo, expected: &[&str]) -> ! {
    use core::fmt::Write;
    use osel_core::exception::FixedBuf;

    crate::avganec::panic_screen(info);
    // formatted without the heap: the panic may come from the allocator
    let mut message = FixedBuf::<2048>::new();
    let _ = write!(message, "{}", info.message());
    if let Some(missing) = expected.iter().find(|part| !message.as_str().contains(*part)) {
        serial_println!("[failed]\n\nError: `{}` is not in the panic message:\n{}\n", missing, message.as_str());
        exit_qemu(QemuExitCode::Failed);
    }
    let has_screen = crate::avganec::with_console(|_| ()).is_some();
    if has_screen && crate::avganec::read_pixel(0, 0) != Some(crate::avganec::Color::RED) {
        fail("the panic screen is not red");
    }
    pass();
}

/// Defines a `harness = false` integration test that boots the kernel, runs `$body`
/// and passes only if the body panics with all `$expected` parts in the message.
#[macro_export]
macro_rules! panic_test {
    ($name:literal, [$($expected:expr),* $(,)?], $body:block) => {
        $crate::bootloader_api::entry_point!(__panic_test_main, config = &$crate::BOOTLOADER_CONFIG);

        #[allow(unreachable_code)]
        fn __panic_test_main(boot_info: &'static mut $crate::bootloader_api::BootInfo) -> ! {
            $crate::serial_print!("{}...\t", $name);
            $crate::avganec::init(boot_info.framebuffer.take());
            $crate::init(boot_info);
            $body
            $crate::testosterone::fail("execution continued, no panic");
        }

        #[panic_handler]
        fn panic(info: &core::panic::PanicInfo) -> ! {
            $crate::testosterone::expect_panic(info, &[$($expected),*])
        }
    };
}
