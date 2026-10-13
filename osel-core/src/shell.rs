//! Bounded command-line editing and parsing, independent of terminal hardware.

use alloc::string::String;
use alloc::vec::Vec;

pub const MAX_LINE: usize = 256;
pub const MAX_ARGS: usize = 16;

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    None,
    Echo(u8),
    Erase,
    Cancel,
    Submit,
    Rejected,
}

pub struct LineEditor {
    bytes: [u8; MAX_LINE],
    len: usize,
    limit: usize,
    rejected: bool,
    escape: u8,
    cr: bool,
}

impl LineEditor {
    pub fn new(limit: usize) -> Self {
        Self {
            bytes: [0; MAX_LINE],
            len: 0,
            limit: limit.min(MAX_LINE),
            rejected: false,
            escape: 0,
            cr: false,
        }
    }

    pub fn text(&self) -> &str {
        // Only printable ASCII and spaces enter the buffer.
        core::str::from_utf8(&self.bytes[..self.len]).unwrap()
    }

    pub fn clear(&mut self) {
        self.len = 0;
        self.rejected = false;
        self.escape = 0;
    }

    pub fn receive(&mut self, byte: u8) -> Event {
        let previous_cr = self.cr;
        self.cr = byte == b'\r';
        if byte == b'\n' && previous_cr {
            return Event::None;
        }
        if byte == 3 || byte == 21 {
            self.clear();
            return Event::Cancel;
        }
        if byte == b'\r' || byte == b'\n' {
            self.escape = 0;
            return if self.rejected {
                Event::Rejected
            } else {
                Event::Submit
            };
        }
        if byte == 27 {
            self.escape = 1;
            return Event::None;
        }
        if self.escape != 0 {
            self.escape = match (self.escape, byte) {
                (1, b'[' | b'O') => 2,
                (2, 0x20..=0x3f) => 2,
                _ => 0,
            };
            return Event::None;
        }
        if byte == 8 || byte == 127 {
            if self.len == 0 {
                return Event::None;
            }
            self.len -= 1;
            return Event::Erase;
        }
        let byte = if byte == b'\t' { b' ' } else { byte };
        if byte >= 128 {
            self.rejected = true;
            return Event::None;
        }
        if !(b' '..=b'~').contains(&byte) {
            return Event::None;
        }
        if self.len == self.limit {
            self.rejected = true;
            return Event::None;
        }
        self.bytes[self.len] = byte;
        self.len += 1;
        Event::Echo(byte)
    }
}

#[derive(Debug, PartialEq, Eq)]
pub enum ParseError {
    TooLong,
    TooManyArgs,
    UnsupportedSyntax,
}

/// No expansion, quoting, redirection or pipelines yet. Reject syntax rather
/// than silently executing only a prefix of the user's command.
pub fn parse(line: &str) -> Result<Vec<&str>, ParseError> {
    if line.len() > MAX_LINE {
        return Err(ParseError::TooLong);
    }
    if line
        .bytes()
        .any(|b| !matches!(b, b'\t' | b' '..=b'~') || b"\"'\\|&;<>`$".contains(&b))
    {
        return Err(ParseError::UnsupportedSyntax);
    }
    let args: Vec<_> = line.split_ascii_whitespace().collect();
    if args.len() > MAX_ARGS {
        return Err(ParseError::TooManyArgs);
    }
    Ok(args)
}

pub fn executable(command: &str) -> String {
    if command.contains('/') {
        command.into()
    } else {
        alloc::format!("/bin/{command}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn editing_and_crlf() {
        let mut line = LineEditor::new(20);
        for &b in b"hellp\x08o" {
            line.receive(b);
        }
        assert_eq!(line.text(), "hello");
        assert_eq!(line.receive(b'\r'), Event::Submit);
        line.clear();
        assert_eq!(line.receive(b'\n'), Event::None);
        assert_eq!(line.receive(127), Event::None);
        line.receive(b'x');
        assert_eq!(line.receive(3), Event::Cancel);
        assert_eq!(line.text(), "");
    }

    #[test]
    fn overflow_never_executes_a_truncated_command() {
        let mut line = LineEditor::new(3);
        for &b in b"abcd\x08" {
            line.receive(b);
        }
        assert_eq!(line.receive(b'\n'), Event::Rejected);
        line.clear();
        line.receive(b'a');
        assert_eq!(line.receive(b'\n'), Event::Submit);
        assert_eq!(line.text(), "a");
    }

    #[test]
    fn escape_sequences_do_not_insert_arrow_names() {
        let mut line = LineEditor::new(MAX_LINE);
        for &b in b"he\x1b[A\x1b[1;5D\x1bOHlp" {
            line.receive(b);
        }
        assert_eq!(line.text(), "help");
        line.receive(0xff);
        assert_eq!(line.receive(b'\n'), Event::Rejected);
        assert_eq!(line.receive(21), Event::Cancel);
        assert_eq!(line.receive(b'\n'), Event::Submit);
    }

    #[test]
    fn parsing_limits_and_paths() {
        assert_eq!(
            parse("  tetris\t--seed 2 ").unwrap(),
            ["tetris", "--seed", "2"]
        );
        assert!(parse(" \t ").unwrap().is_empty());
        assert_eq!(parse(&"x".repeat(MAX_LINE + 1)), Err(ParseError::TooLong));
        assert_eq!(
            parse(&"x ".repeat(MAX_ARGS + 1)),
            Err(ParseError::TooManyArgs)
        );
        for s in ["a|b", "a >b", "a;b", "'a'", "a\0", "Привет"] {
            assert_eq!(parse(s), Err(ParseError::UnsupportedSyntax));
        }
        assert_eq!(executable("clear"), "/bin/clear");
        assert_eq!(executable("/bin/hello"), "/bin/hello");
        assert_eq!(executable("bin/hello"), "bin/hello");
    }
}
