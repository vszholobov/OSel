//! Keyboard input: PS/2 scancode decoding and the buffer of typed characters

use pc_keyboard::{DecodedKey, HandleControl, KeyCode, KeyState, PS2Keyboard, ScancodeSet1, layouts};

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

    /// Drops all items, the `dropped` count stays: it is a statistic since boot
    pub fn clear(&mut self) {
        self.head = 0;
        self.len = 0;
    }
}

/// Input events retain navigation keys without inventing Unicode characters.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Character(char),
    Sequence(&'static str),
    PageUp,
    PageDown,
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
        match self.decode_key(scancode)? {
            Key::Character(c) => Some(c),
            _ => None,
        }
    }

    /// Navigation is encoded for terminals; Ctrl+Shift+Up/Down also scroll locally.
    /// Key releases always update modifier state but never emit input.
    pub fn decode_key(&mut self, scancode: u8) -> Option<Key> {
        let event = self.keyboard.add_byte(scancode).ok()??;
        let code = event.code;
        let state = event.state;
        let decoded = self.keyboard.process_keyevent(event);
        if state == KeyState::Up { return None; }
        let modifiers = self.keyboard.get_modifiers();
        let scroll = (modifiers.lctrl || modifiers.rctrl) && (modifiers.lshift || modifiers.rshift);
        match code {
            KeyCode::PageUp => Some(Key::PageUp),
            KeyCode::PageDown => Some(Key::PageDown),
            KeyCode::ArrowUp if scroll => Some(Key::PageUp),
            KeyCode::ArrowDown if scroll => Some(Key::PageDown),
            KeyCode::ArrowUp => Some(Key::Sequence("\x1b[A")),
            KeyCode::ArrowDown => Some(Key::Sequence("\x1b[B")),
            KeyCode::ArrowLeft => Some(Key::Sequence("\x1b[D")),
            KeyCode::ArrowRight => Some(Key::Sequence("\x1b[C")),
            _ => match decoded? {
                DecodedKey::Unicode(c) => Some(Key::Character(c)),
                DecodedKey::RawKey(_) => None,
            },
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
    fn navigation_and_scroll_shortcuts_preserve_modifiers_and_ignore_releases() {
        let mut decoder = ScancodeDecoder::new();
        let mut keys = |codes: &[u8]| -> Vec<Key> {
            codes.iter().filter_map(|&code| decoder.decode_key(code)).collect()
        };
        assert_eq!(keys(&[0xe0, 0x48, 0xe0, 0xc8, 0xe0, 0x50, 0xe0, 0xd0]),
            [Key::Sequence("\x1b[A"), Key::Sequence("\x1b[B")]);
        assert_eq!(keys(&[0xe0, 0x49, 0xe0, 0xc9, 0xe0, 0x51, 0xe0, 0xd1]),
            [Key::PageUp, Key::PageDown]);
        assert_eq!(keys(&[0x1d, 0x2a, 0xe0, 0x48, 0xe0, 0xc8, 0xe0, 0x50, 0xe0, 0xd0,
            0xaa, 0x9d, 0x1e, 0x9e]), [Key::PageUp, Key::PageDown, Key::Character('a')]);
        assert_eq!(keys(&[0xe0, 0x1d, 0x36, 0xe0, 0x48, 0xe0, 0xc8, 0xb6, 0xe0, 0x9d,
            0xe0, 0x48, 0xe0, 0xc8]), [Key::PageUp, Key::Sequence("\x1b[A")]);
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
    fn ring_buffer_without_capacity() {
        let mut buffer = RingBuffer::<u8, 0>::new(0);
        assert!(!buffer.push(1));
        assert_eq!(buffer.dropped(), 1);
        assert_eq!(buffer.pop(), None);
    }

    #[test]
    fn ring_buffer_of_one() {
        let mut buffer = RingBuffer::<u8, 1>::new(0);
        for i in 0..5 {
            assert!(buffer.push(i));
            assert!(!buffer.push(100));
            assert_eq!(buffer.pop(), Some(i));
        }
        assert_eq!(buffer.dropped(), 5);
    }

    #[test]
    fn clear_keeps_the_dropped_count() {
        let mut buffer = RingBuffer::<u8, 1>::new(0);
        buffer.push(1);
        buffer.push(2);
        buffer.clear();
        assert_eq!(buffer.dropped(), 1);
    }

    #[test]
    fn pause_and_print_screen_leave_no_modifiers_behind() {
        // Pause, then c: a stuck Ctrl from E1 1D would turn c into \x03
        assert_eq!(type_fresh(&[0xe1, 0x1d, 0x45, 0xe1, 0x9d, 0xc5, 0x2e, 0xae]), "c");
        // Print Screen press and release, then c
        assert_eq!(type_fresh(&[0xe0, 0x2a, 0xe0, 0x37, 0xe0, 0xb7, 0xe0, 0xaa, 0x2e, 0xae]), "c");
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
