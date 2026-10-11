//! Keyboard input: PS/2 scancode decoding and the buffer of typed characters

use pc_keyboard::{DecodedKey, HandleControl, PS2Keyboard, ScancodeSet1, layouts};

/// Fixed capacity FIFO queue that drops new items when full
pub struct RingBuffer<T: Copy, const N: usize> {
    items: [T; N],
    head: usize,
    len: usize,
    dropped: usize,
}

impl<T: Copy, const N: usize> RingBuffer<T, N> {
    /// `fill` only initializes the storage, it is never returned
    pub const fn new(fill: T) -> Self {
        RingBuffer { items: [fill; N], head: 0, len: 0, dropped: 0 }
    }

    /// Appends `item`, returns `false` and counts it as dropped when the buffer is full
    pub fn push(&mut self, item: T) -> bool {
        if self.len == N {
            self.dropped += 1;
            return false;
        }
        self.items[(self.head + self.len) % N] = item;
        self.len += 1;
        true
    }

    /// Takes the oldest item
    pub fn pop(&mut self) -> Option<T> {
        if self.len == 0 {
            return None;
        }
        let item = self.items[self.head];
        self.head = (self.head + 1) % N;
        self.len -= 1;
        Some(item)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub const fn capacity(&self) -> usize {
        N
    }

    /// Number of items lost because the buffer was full
    pub fn dropped(&self) -> usize {
        self.dropped
    }

    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

/// Turns PS/2 scancode set 1 bytes into characters, US layout.
/// Ctrl+letter gives the control character (Ctrl+C is `\x03`) like a terminal.
pub struct ScancodeDecoder {
    keyboard: PS2Keyboard<layouts::Us104Key, ScancodeSet1>,
}

impl ScancodeDecoder {
    pub const fn new() -> Self {
        ScancodeDecoder {
            keyboard: PS2Keyboard::new(ScancodeSet1::new(), layouts::Us104Key, HandleControl::MapLettersToUnicode),
        }
    }

    /// Feeds one byte from the keyboard, returns a character once a key press is complete.
    /// Releases, modifiers, keys without a character and invalid bytes give `None`.
    pub fn decode(&mut self, scancode: u8) -> Option<char> {
        let event = self.keyboard.add_byte(scancode).ok()??;
        match self.keyboard.process_keyevent(event)? {
            DecodedKey::Unicode(c) => Some(c),
            DecodedKey::RawKey(_) => None,
        }
    }
}

impl Default for ScancodeDecoder {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::String;
    use alloc::vec::Vec;

    fn typed(decoder: &mut ScancodeDecoder, scancodes: &[u8]) -> String {
        scancodes.iter().filter_map(|&s| decoder.decode(s)).collect()
    }

    fn type_fresh(scancodes: &[u8]) -> String {
        typed(&mut ScancodeDecoder::new(), scancodes)
    }

    #[test]
    fn ring_buffer_is_fifo() {
        let mut buffer = RingBuffer::<u32, 4>::new(0);
        assert!(buffer.is_empty());
        assert_eq!(buffer.pop(), None);
        for i in 1..=3 {
            assert!(buffer.push(i));
        }
        assert_eq!(buffer.len(), 3);
        assert_eq!((buffer.pop(), buffer.pop(), buffer.pop(), buffer.pop()), (Some(1), Some(2), Some(3), None));
    }

    #[test]
    fn ring_buffer_drops_when_full() {
        let mut buffer = RingBuffer::<char, 3>::new('\0');
        assert_eq!(buffer.capacity(), 3);
        assert!(buffer.push('a') && buffer.push('b') && buffer.push('c'));
        assert!(!buffer.push('d'));
        assert!(!buffer.push('e'));
        assert_eq!(buffer.dropped(), 2);
        assert_eq!(buffer.len(), 3);
        assert_eq!(buffer.pop(), Some('a'), "old items are kept, new ones are dropped");
        assert!(buffer.push('f'));
        let rest: Vec<char> = core::iter::from_fn(|| buffer.pop()).collect();
        assert_eq!(rest, ['b', 'c', 'f']);
    }

    #[test]
    fn ring_buffer_wraps_around() {
        let mut buffer = RingBuffer::<usize, 5>::new(0);
        let mut next_in = 0;
        let mut next_out = 0;
        for round in 0..50 {
            for _ in 0..(round % 5) + 1 {
                if buffer.push(next_in) {
                    next_in += 1;
                }
            }
            for _ in 0..(round % 3) + 1 {
                if let Some(item) = buffer.pop() {
                    assert_eq!(item, next_out);
                    next_out += 1;
                }
            }
        }
        assert_eq!(next_in - next_out, buffer.len());
    }

    #[test]
    fn ring_buffer_clear() {
        let mut buffer = RingBuffer::<u8, 2>::new(0);
        buffer.push(1);
        buffer.push(2);
        buffer.clear();
        assert!(buffer.is_empty());
        assert!(buffer.push(3));
        assert_eq!(buffer.pop(), Some(3));
    }

    #[test]
    fn letters_and_digits() {
        // a press, a release, 1 press, 1 release
        assert_eq!(type_fresh(&[0x1e, 0x9e, 0x02, 0x82]), "a1");
    }

    #[test]
    fn release_alone_gives_nothing() {
        assert_eq!(type_fresh(&[0x9e, 0x82]), "");
    }

    #[test]
    fn shift_gives_capitals_until_released() {
        let mut decoder = ScancodeDecoder::new();
        // left shift down, a, 1, left shift up, a
        assert_eq!(typed(&mut decoder, &[0x2a, 0x1e, 0x9e, 0x02, 0x82, 0xaa, 0x1e, 0x9e]), "A!a");
    }

    #[test]
    fn caps_lock_toggles() {
        let mut decoder = ScancodeDecoder::new();
        assert_eq!(typed(&mut decoder, &[0x3a, 0xba, 0x1e, 0x9e]), "A");
        assert_eq!(typed(&mut decoder, &[0x3a, 0xba, 0x1e, 0x9e]), "a");
    }

    #[test]
    fn special_keys() {
        assert_eq!(type_fresh(&[0x1c, 0x9c]), "\n", "enter");
        assert_eq!(type_fresh(&[0x0e, 0x8e]), "\u{8}", "backspace");
        assert_eq!(type_fresh(&[0x0f, 0x8f]), "\t", "tab");
        assert_eq!(type_fresh(&[0x39, 0xb9]), " ", "space");
        assert_eq!(type_fresh(&[0x01, 0x81]), "\u{1b}", "escape");
    }

    #[test]
    fn extended_keys() {
        // keypad enter is E0 1C
        assert_eq!(type_fresh(&[0xe0, 0x1c, 0xe0, 0x9c]), "\n");
        // arrows have no character, and must not break the following keys
        assert_eq!(type_fresh(&[0xe0, 0x4b, 0xe0, 0xcb, 0x1e, 0x9e]), "a");
    }

    #[test]
    fn ctrl_letters_are_control_characters() {
        // left ctrl down, c, ctrl up
        assert_eq!(type_fresh(&[0x1d, 0x2e, 0xae, 0x9d]), "\u{3}");
    }

    #[test]
    fn invalid_bytes_do_not_break_decoding() {
        // controller replies like ACK (0xFA) or errors (0xFF) may reach the data port
        assert_eq!(type_fresh(&[0xfa, 0x1e, 0x9e, 0xff, 0x02, 0x82]), "a1");
    }

    #[test]
    fn every_letter() {
        let scancodes: [u8; 26] = [
            0x1e, 0x30, 0x2e, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19, 0x10,
            0x13, 0x1f, 0x14, 0x16, 0x2f, 0x11, 0x2d, 0x15, 0x2c,
        ];
        let presses: Vec<u8> = scancodes.iter().flat_map(|&s| [s, s | 0x80]).collect();
        assert_eq!(type_fresh(&presses), "abcdefghijklmnopqrstuvwxyz");
    }
}
