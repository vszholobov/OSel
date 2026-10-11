//! cereal module provides serial port access methods

use core::ops::Deref;
use lazy_static::lazy_static;
use spin::Mutex;
use uart_16550::{Config, Uart16550Tty, backend::PioBackend};

#[doc(hidden)]
pub fn _print(args: core::fmt::Arguments) {
    use core::fmt::Write;
    // an interrupt handler printing while we hold the lock would deadlock
    x86_64::instructions::interrupts::without_interrupts(|| {
        if let Some(port) = SERIAL1.deref().lock().as_mut() {
            let _ = port.write_fmt(args);
        }
    });
}

/// Writes straight to COM1 without locks or initialization, for panics where the
/// normal path may be broken. Gives up quickly when there is no serial port.
pub fn emergency_print(args: core::fmt::Arguments) {
    use x86_64::instructions::port::Port;
    struct RawCom1;
    impl core::fmt::Write for RawCom1 {
        fn write_str(&mut self, s: &str) -> core::fmt::Result {
            let mut line_status = Port::<u8>::new(0x3fd);
            let mut data = Port::<u8>::new(0x3f8);
            for byte in s.bytes() {
                // transmitter empty, bounded so a missing port can't hang the panic
                for _ in 0..100_000 {
                    if unsafe { line_status.read() } & 0x20 != 0 {
                        break;
                    }
                }
                unsafe { data.write(byte) };
            }
            Ok(())
        }
    }
    let _ = core::fmt::Write::write_fmt(&mut RawCom1, args);
}

/// Prints to the host through the serial interface.
#[macro_export]
macro_rules! serial_print {
    ($($arg:tt)*) => ($crate::cereal::_print(format_args!($($arg)*)));
}

/// Prints to the host through the serial interface, appending a newline.
#[macro_export]
macro_rules! serial_println {
    () => ($crate::serial_print!("\n"));
    ($fmt:expr) => ($crate::serial_print!(concat!($fmt, "\n")));
    ($fmt:expr, $($arg:tt)*) => ($crate::serial_print!(
        concat!($fmt, "\n"), $($arg)*));
}

lazy_static! {
    /// COM1, `None` when the machine has no working serial port (most laptops):
    /// printing to it is skipped then instead of failing
    pub static ref SERIAL1: Mutex<Option<Uart16550Tty<PioBackend>>> =
        Mutex::new(unsafe { Uart16550Tty::new_port(0x3F8, Config::default()) }.ok());
}

#[cfg(test)]
mod tests {
    #[test_case]
    fn test_serial_port_is_present_in_qemu() {
        assert!(super::SERIAL1.lock().is_some());
    }

    #[test_case]
    fn test_serial_print_in_expression() {
        let value = Some(1).map(|_| serial_print!("")).map(|_| 2);
        assert_eq!(value, Some(2));
    }
}
