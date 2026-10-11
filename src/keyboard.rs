//! keyboard module decodes PS/2 scancodes and buffers typed characters

use pc_keyboard::{DecodedKey, HandleControl, PS2Keyboard, ScancodeSet1, layouts};
use spin::Mutex;
use x86_64::instructions::interrupts;

static KEYBOARD: Mutex<PS2Keyboard<layouts::Us104Key, ScancodeSet1>> = Mutex::new(PS2Keyboard::new(
    ScancodeSet1::new(),
    layouts::Us104Key,
    HandleControl::Ignore,
));

const BUFFER_SIZE: usize = 64;

/// Ring buffer of typed characters, filled by the keyboard interrupt handler
struct CharBuffer {
    chars: [char; BUFFER_SIZE],
    head: usize,
    len: usize,
}

static BUFFER: Mutex<CharBuffer> = Mutex::new(CharBuffer {
    chars: ['\0'; BUFFER_SIZE],
    head: 0,
    len: 0,
});

/// Called from the keyboard interrupt handler with a raw scancode from port 0x60
pub fn handle_scancode(scancode: u8) {
    let mut keyboard = KEYBOARD.lock();
    let Ok(Some(key_event)) = keyboard.add_byte(scancode) else {
        return;
    };
    if let Some(DecodedKey::Unicode(c)) = keyboard.process_keyevent(key_event) {
        let mut buffer = BUFFER.lock();
        // drop the input when nobody reads it
        if buffer.len < BUFFER_SIZE {
            let tail = (buffer.head + buffer.len) % BUFFER_SIZE;
            buffer.chars[tail] = c;
            buffer.len += 1;
        }
    }
}

/// Takes the oldest typed character, if any
pub fn pop_char() -> Option<char> {
    // the buffer is shared with the interrupt handler, so it must not fire while we hold the lock
    interrupts::without_interrupts(|| {
        let mut buffer = BUFFER.lock();
        if buffer.len == 0 {
            return None;
        }
        let c = buffer.chars[buffer.head];
        buffer.head = (buffer.head + 1) % BUFFER_SIZE;
        buffer.len -= 1;
        Some(c)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use x86_64::instructions::port::Port;

    /// Makes the PS/2 controller deliver `scancode` as if it came from the keyboard
    fn inject_scancode(scancode: u8) {
        let mut command: Port<u8> = Port::new(0x64);
        let mut data: Port<u8> = Port::new(0x60);
        unsafe {
            // wait until the interrupt handler has consumed the previous byte
            while command.read() & 0b01 != 0 {
                x86_64::instructions::hlt();
            }
            while command.read() & 0b10 != 0 {}
            command.write(0xd2); // write keyboard output buffer
            while command.read() & 0b10 != 0 {}
            data.write(scancode);
        }
    }

    fn wait_char() -> char {
        let start = crate::interrupts::ticks();
        loop {
            if let Some(c) = pop_char() {
                return c;
            }
            assert!(crate::interrupts::ticks() < start + 20, "no key from keyboard interrupt");
            x86_64::instructions::hlt();
        }
    }

    #[test_case]
    fn test_keyboard_interrupt() {
        while pop_char().is_some() {}
        // press and release `a`, then `1`
        for scancode in [0x1e, 0x9e, 0x02, 0x82] {
            inject_scancode(scancode);
        }
        assert_eq!(wait_char(), 'a');
        assert_eq!(wait_char(), '1');
    }
}
