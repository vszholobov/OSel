//! keyboard module decodes PS/2 scancodes and buffers typed characters

use osel_core::keyboard::{RingBuffer, ScancodeDecoder};
use crate::sync::IrqSpinlock;

pub const BUFFER_SIZE: usize = 64;

static DECODER: IrqSpinlock<ScancodeDecoder> = IrqSpinlock::new("DECODER", ScancodeDecoder::new());

/// Typed characters, filled by the keyboard interrupt handler
static BUFFER: IrqSpinlock<RingBuffer<char, BUFFER_SIZE>> = IrqSpinlock::new("BUFFER", RingBuffer::new('\0'));

/// Called from the keyboard interrupt handler with a raw scancode from port 0x60
pub fn handle_scancode(scancode: u8) {
    if let Some(c) = DECODER.lock().decode(scancode) {
        // when nobody reads the input it is dropped
        BUFFER.lock().push(c);
    }
}

/// Takes the oldest typed character, if any
pub fn pop_char() -> Option<char> {
    BUFFER.lock().pop()
}

/// Number of characters lost because nobody read them in time
pub fn dropped_chars() -> usize {
    BUFFER.lock().dropped()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interrupts::ticks;
    use alloc::string::String;
    use x86_64::instructions::port::Port;

    const STATUS_OUTPUT_FULL: u8 = 0b01;
    const STATUS_INPUT_FULL: u8 = 0b10;
    /// About half a second at 100 Hz
    const TIMEOUT_TICKS: u64 = 50;

    /// Waits until `done` returns true, panics instead of hanging the test run
    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let start = ticks();
        while !done() {
            assert!(ticks() < start + TIMEOUT_TICKS, "timed out waiting for {}", what);
            x86_64::instructions::hlt();
        }
    }

    fn status() -> u8 {
        let mut command: Port<u8> = Port::new(0x64);
        unsafe { command.read() }
    }

    /// Makes the PS/2 controller deliver `scancode` as if it came from the keyboard
    fn inject_scancode(scancode: u8) {
        let mut command: Port<u8> = Port::new(0x64);
        let mut data: Port<u8> = Port::new(0x60);
        // the interrupt handler has to consume the previous byte first
        wait_until("keyboard interrupt to read the previous byte", || status() & STATUS_OUTPUT_FULL == 0);
        wait_until("controller to accept a command", || status() & STATUS_INPUT_FULL == 0);
        unsafe { command.write(0xd2) }; // write keyboard output buffer
        wait_until("controller to accept data", || status() & STATUS_INPUT_FULL == 0);
        unsafe { data.write(scancode) };
    }

    fn inject_all(scancodes: &[u8]) {
        for &scancode in scancodes {
            inject_scancode(scancode);
        }
        wait_until("the last byte to be handled", || status() & STATUS_OUTPUT_FULL == 0);
    }

    fn drain() -> String {
        core::iter::from_fn(pop_char).collect()
    }

    #[test_case]
    fn test_keyboard_interrupt() {
        drain();
        inject_all(&[0x1e, 0x9e, 0x02, 0x82]);
        assert_eq!(drain(), "a1");
    }

    #[test_case]
    fn test_keyboard_shift() {
        drain();
        inject_all(&[0x2a, 0x1e, 0x9e, 0xaa, 0x1e, 0x9e]);
        assert_eq!(drain(), "Aa");
    }

    #[test_case]
    fn test_keyboard_extended_keys_are_skipped() {
        drain();
        // left arrow press and release, then a
        inject_all(&[0xe0, 0x4b, 0xe0, 0xcb, 0x1e, 0x9e]);
        assert_eq!(drain(), "a");
    }

    #[test_case]
    fn test_keyboard_buffer_overflow_drops_new_chars() {
        drain();
        let dropped_before = dropped_chars();
        for _ in 0..BUFFER_SIZE + 6 {
            inject_all(&[0x1e, 0x9e]);
        }
        assert_eq!(drain().len(), BUFFER_SIZE);
        assert_eq!(dropped_chars() - dropped_before, 6);
    }
}
