//! aVGAnec module provides basic text showing methods on top of the linear framebuffer
//!
//! Works the same way under UEFI (GOP) and BIOS (VESA). The drawing itself lives in
//! `osel_core::console`, here is only the global console and the print macros.

use bootloader_api::info::FrameBuffer;
use core::fmt;
use core::panic::PanicInfo;
use osel_core::console::Console;
use crate::sync::IrqSpinlock;
use x86_64::instructions::interrupts;

pub use osel_core::console::Color;

#[macro_export]
macro_rules! print {
    ($($arg:tt)*) => ($crate::avganec::_print(format_args!($($arg)*)));
}

#[macro_export]
macro_rules! println {
    () => ($crate::print!("\n"));
    ($($arg:tt)*) => ($crate::print!("{}\n", format_args!($($arg)*)));
}

#[doc(hidden)]
pub fn _print(args: fmt::Arguments) {
    use core::fmt::Write;
    // printing before `init` or without a framebuffer is a no-op
    if let Some(console) = WRITER.lock().as_mut() {
        let _ = console.write_fmt(args);
    }
}

/// Prints unless the console is busy: for exception handlers, which may interrupt
/// code holding the console lock (interrupts being off doesn't stop exceptions)
pub fn try_print(args: fmt::Arguments) {
    use core::fmt::Write;
    if let Some(mut console) = WRITER.try_lock()
        && let Some(console) = console.as_mut()
    {
        let _ = console.write_fmt(args);
    }
}

pub static WRITER: IrqSpinlock<Option<Console<'static>>> = IrqSpinlock::new("WRITER", None);

/// Takes over the framebuffer handed over by the bootloader.
/// An unusable framebuffer is reported to the serial port, printing then goes nowhere.
pub fn init(framebuffer: Option<FrameBuffer>) {
    let console = framebuffer.and_then(|fb| {
        let info = fb.info();
        Console::new(fb.into_buffer(), info)
            .map_err(|error| crate::serial_println!("avganec: framebuffer {:?} is not usable: {:?}", info, error))
            .ok()
    });
    *WRITER.lock() = console;
}

/// Runs `f` with the console (and interrupts off), `None` without a framebuffer
#[track_caller]
pub fn with_console<R>(f: impl FnOnce(&mut Console<'static>) -> R) -> Option<R> {
    WRITER.lock().as_mut().map(f)
}

/// Color of the pixel at (`x`, `y`), `None` without a framebuffer or outside of the screen
pub fn read_pixel(x: usize, y: usize) -> Option<Color> {
    with_console(|console| console.read_pixel(x, y)).flatten()
}

/// Shows a fatal error white on red over the whole screen and mirrors it to the serial port.
///
/// A panic while showing a panic (say, a broken `Display` in the message) only gets a raw
/// line on the serial port: drawing again would most likely panic again. Returns `false`
/// in that case, the caller must not touch the screen or serial locks anymore then.
pub fn panic_screen(info: &PanicInfo) -> bool {
    use core::fmt::Write;
    interrupts::disable();
    if !crate::enter_panic() {
        crate::cereal::emergency_print(format_args!("\nnested panic: {}\n", info));
        return false;
    }
    // the panic could happen while printing, nobody would release this lock anymore
    unsafe { WRITER.force_unlock() };
    if let Some(console) = WRITER.lock().as_mut() {
        console.set_colors(Color::WHITE, Color::RED);
        console.clear();
        let _ = write!(console, "OSel crashed :(\nOSel {}, commit {}\n\n{}\n", crate::VERSION, crate::COMMIT, info);
    }
    // the framebuffer is never read back by the kernel, make sure the writes happen
    core::sync::atomic::compiler_fence(core::sync::atomic::Ordering::SeqCst);
    unsafe { crate::cereal::SERIAL1.force_unlock() };
    crate::serial_println!("OSel {} (commit {}) crashed: {}", crate::VERSION, crate::COMMIT, info);
    true
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_println_simple() {
        println!("test_println_simple output");
        let shown = with_console(|console| {
            let row = console.cursor().0 - 1;
            "test_println_simple output".chars().enumerate().all(|(i, c)| console.cell_shows(row, i, c))
        });
        assert_eq!(shown, Some(true));
    }

    #[test_case]
    fn test_console_lock_disables_interrupts() {
        with_console(|_| assert!(!interrupts::are_enabled()));
        assert!(interrupts::are_enabled());
    }

    #[test_case]
    fn test_println_many() {
        for _ in 0..200 {
            println!("test_println_many output");
        }
        let (row, column) = with_console(|c| c.cursor()).expect("no framebuffer");
        let rows = with_console(|c| c.rows()).unwrap();
        assert_eq!((row, column), (rows - 1, 0), "the screen is full and scrolled");
    }

    #[test_case]
    fn test_println_output() {
        let s = "Some test string that fits on a single line";
        println!("{}", s);
        let shown = with_console(|console| {
            // println moved the cursor to the start of the next line
            let row = console.cursor().0 - 1;
            s.chars().enumerate().all(|(i, c)| console.cell_shows(row, i, c))
        });
        assert_eq!(shown, Some(true));
    }

    #[test_case]
    fn test_print_without_newline_continues_line() {
        println!();
        print!("ab");
        print!("cd");
        let shown = with_console(|console| {
            let (row, column) = console.cursor();
            column == 4 && "abcd".chars().enumerate().all(|(i, c)| console.cell_shows(row, i, c))
        });
        assert_eq!(shown, Some(true));
        println!();
    }

    #[test_case]
    fn test_read_pixel_bounds() {
        let info = with_console(|c| c.info()).expect("no framebuffer");
        assert!(read_pixel(0, 0).is_some());
        assert!(read_pixel(info.width - 1, info.height - 1).is_some());
        assert_eq!(read_pixel(info.width, 0), None);
        assert_eq!(read_pixel(0, info.height), None);
    }

    #[test_case]
    fn test_screen_is_big_enough() {
        let (columns, rows) = with_console(|c| (c.columns(), c.rows())).expect("no framebuffer");
        assert!(columns >= 80 && rows >= 25, "{}x{} text cells", columns, rows);
    }
}
