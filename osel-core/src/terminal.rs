//! Linux terminal settings and bounded input buffering, independent of hardware.

use crate::errno::{Errno, KResult};

pub const TERMIOS_SIZE: usize = 36;
pub const ICANON: u32 = 2;
pub const ECHO: u32 = 8;
const ICRNL: u32 = 0x100;
const IGNCR: u32 = 0x80;
const INLCR: u32 = 0x40;
const CAPACITY: usize = 1024;
const EOF: u16 = 256;

/// The kernel ABI has 19 control characters, unlike libc's larger termios structure.
#[derive(Clone, Copy)]
pub struct Termios(pub [u8; TERMIOS_SIZE]);

impl Termios {
    /// Preserve OSel's immediate, no-echo input at boot. Programs can request canonical input.
    pub const fn new() -> Self {
        let mut bytes = [0; TERMIOS_SIZE];
        bytes[8] = 0xbf; // B38400 | CS8 | CREAD
        bytes[19] = 127; // VERASE
        bytes[20] = 21; // VKILL
        bytes[21] = 4; // VEOF
        bytes[23] = 1; // VMIN
        Self(bytes)
    }

    pub fn local_flags(&self) -> u32 {
        u32::from_le_bytes(self.0[12..16].try_into().unwrap())
    }

    pub fn canonical(&self) -> bool {
        self.local_flags() & ICANON != 0
    }

    pub fn minimum(&self) -> usize {
        self.0[23] as usize
    }
}

pub struct Input {
    pub settings: Termios,
    bytes: [u16; CAPACITY],
    head: usize,
    len: usize,
}

impl Input {
    pub const fn new() -> Self {
        Self {
            settings: Termios::new(),
            bytes: [0; CAPACITY],
            head: 0,
            len: 0,
        }
    }

    pub fn configure(&mut self, settings: Termios, flush: bool) -> KResult<()> {
        // Timed reads and signal generation need their own scheduler/signal integration.
        if settings.0[22] != 0 || settings.0[16] != 0 {
            return Err(Errno::EINVAL);
        }
        self.settings = settings;
        if flush {
            self.head = 0;
            self.len = 0;
        }
        Ok(())
    }

    /// Returns bytes to echo. The caller prints them outside any interrupt or input lock.
    pub fn receive(&mut self, mut byte: u8) -> ([u8; 3], usize) {
        let flags = u32::from_le_bytes(self.settings.0[..4].try_into().unwrap());
        if byte == b'\r' {
            if flags & IGNCR != 0 {
                return ([0; 3], 0);
            }
            if flags & ICRNL != 0 {
                byte = b'\n';
            }
        } else if byte == b'\n' && flags & INLCR != 0 {
            byte = b'\r';
        }
        if self.settings.canonical() && (byte == self.settings.0[19] || byte == 8) {
            if self.len != 0 {
                let last = self.bytes[(self.head + self.len - 1) % CAPACITY];
                if last != b'\n' as u16 && last != EOF {
                    self.len -= 1;
                    if self.echo() {
                        return (*b"\x08 \x08", 3);
                    }
                }
            }
            return ([0; 3], 0);
        }
        let value = if self.settings.canonical() && byte == self.settings.0[21] {
            EOF
        } else {
            byte as u16
        };
        // Reserve one slot for a line terminator: a full edited line must remain readable.
        if self.len < CAPACITY && (self.len < CAPACITY - 1 || value == EOF || byte == b'\n') {
            self.bytes[(self.head + self.len) % CAPACITY] = value;
            self.len += 1;
            if self.echo() && value != EOF {
                return ([byte, 0, 0], 1);
            }
        }
        ([0; 3], 0)
    }

    pub fn echo(&self) -> bool {
        self.settings.local_flags() & ECHO != 0
    }

    pub fn available(&self, requested: usize) -> bool {
        if self.settings.canonical() {
            (0..self.len).any(|i| matches!(self.bytes[(self.head + i) % CAPACITY], 10 | EOF))
        } else {
            self.len >= self.settings.minimum().min(requested)
        }
    }

    pub fn has_bytes(&self) -> bool {
        self.len != 0
    }

    pub fn read(&mut self, out: &mut [u8]) -> usize {
        let mut done = 0;
        while done < out.len() && self.len != 0 {
            let value = self.bytes[self.head];
            self.head = (self.head + 1) % CAPACITY;
            self.len -= 1;
            if value == EOF {
                break;
            }
            out[done] = value as u8;
            done += 1;
            if self.settings.canonical() && value == b'\n' as u16 {
                break;
            }
        }
        done
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn raw_input_keeps_utf8_and_short_reads() {
        let mut input = Input::new();
        for byte in "Жa".bytes() {
            input.receive(byte);
        }
        let mut out = [0; 1];
        for expected in "Жa".bytes() {
            assert!(input.available(1));
            assert_eq!(input.read(&mut out), 1);
            assert_eq!(out[0], expected);
        }
        assert!(!input.available(1));
    }

    #[test]
    fn canonical_waits_edits_and_stops_at_each_line() {
        let mut input = Input::new();
        input.settings.0[12] = ICANON as u8;
        for byte in b"ab\x7fc" {
            input.receive(*byte);
        }
        assert!(!input.available(10));
        for byte in b"\nsecond\n" {
            input.receive(*byte);
        }
        let mut out = [0; 32];
        let n = input.read(&mut out);
        assert_eq!(&out[..n], b"ac\n");
        let n = input.read(&mut out);
        assert_eq!(&out[..n], b"second\n");
    }

    #[test]
    fn eof_does_not_hide_following_input_and_flush_discards_input() {
        let mut input = Input::new();
        input.settings.0[12] = ICANON as u8;
        for byte in b"\x04x\n" {
            input.receive(*byte);
        }
        let mut out = [0; 32];
        assert!(input.available(32));
        assert_eq!(input.read(&mut out), 0);
        assert!(input.available(32));
        input.configure(Termios::new(), true).unwrap();
        assert!(!input.has_bytes());
    }

    #[test]
    fn overflow_preserves_room_for_enter_and_read_minimum() {
        let mut input = Input::new();
        input.settings.0[12] = ICANON as u8;
        for _ in 0..CAPACITY * 2 {
            input.receive(b'x');
        }
        input.receive(b'\n');
        assert!(input.available(CAPACITY));
        let mut out = [0; CAPACITY];
        assert_eq!(input.read(&mut out), CAPACITY);
        assert_eq!(out[CAPACITY - 1], b'\n');
        let mut modes = Termios::new();
        modes.0[23] = 2;
        input.configure(modes, true).unwrap();
        input.receive(b'a');
        assert!(!input.available(2));
        assert!(input.available(1));
        modes.0[22] = 1;
        assert_eq!(input.configure(modes, false), Err(Errno::EINVAL));
    }

    #[test]
    fn canonical_echo_and_cr_translation_follow_settings() {
        let mut input = Input::new();
        let mut mode = Termios::new();
        mode.0[12] = (ICANON | ECHO) as u8;
        mode.0[..4].copy_from_slice(&ICRNL.to_le_bytes());
        input.configure(mode, false).unwrap();
        assert_eq!(input.receive(b'x'), ([b'x', 0, 0], 1));
        assert_eq!(input.receive(127), (*b"\x08 \x08", 3));
        assert_eq!(input.receive(b'\r'), ([b'\n', 0, 0], 1));
        let mut out = [0; 8];
        assert_eq!(input.read(&mut out), 1);
        assert_eq!(out[0], b'\n');
        mode.0[..4].copy_from_slice(&IGNCR.to_le_bytes());
        input.configure(mode, true).unwrap();
        assert_eq!(input.receive(b'\r'), ([0; 3], 0));
        assert!(!input.has_bytes());
    }
}
