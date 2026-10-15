//! Bounded command-line editing and parsing, independent of terminal hardware.

use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::{self, Write};
use crate::exception::FixedBuf;

pub const MAX_LINE: usize = 256;
pub const MAX_ARGS: usize = 16;
pub const HISTORY_COMMANDS: usize = 64;

#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    None,
    Echo(u8),
    Erase,
    Redraw,
    PageUp,
    PageDown,
    Cancel,
    Submit,
    Rejected,
}

#[derive(Clone, Copy)]
struct Command {
    bytes: [u8; MAX_LINE],
    len: usize,
}

pub struct LineEditor {
    bytes: [u8; MAX_LINE],
    len: usize,
    limit: usize,
    rejected: bool,
    escape: u8,
    cr: bool,
    params: [u8; 16],
    params_len: usize,
    history: Vec<Command>,
    history_head: usize,
    history_len: usize,
    selected: usize,
    draft: Command,
    draft_rejected: bool,
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
            params: [0; 16],
            params_len: 0,
            history: alloc::vec![Command { bytes: [0; MAX_LINE], len: 0 }; HISTORY_COMMANDS],
            history_head: 0,
            history_len: 0,
            selected: 0,
            draft: Command { bytes: [0; MAX_LINE], len: 0 },
            draft_rejected: false,
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
        self.selected = 0;
        self.draft.len = 0;
    }

    pub fn escape_pending(&self) -> bool {
        self.escape != 0
    }

    pub fn cancel_escape(&mut self) {
        self.escape = 0;
    }

    fn remember(&mut self) {
        if self.text().trim().is_empty() {
            return;
        }
        if self.history_len != 0 {
            let last = (self.history_head + HISTORY_COMMANDS - 1) % HISTORY_COMMANDS;
            if self.history[last].bytes[..self.history[last].len] == self.bytes[..self.len] {
                return;
            }
        }
        self.history[self.history_head] = Command { bytes: self.bytes, len: self.len };
        self.history_head = (self.history_head + 1) % HISTORY_COMMANDS;
        self.history_len = (self.history_len + 1).min(HISTORY_COMMANDS);
    }

    fn recall(&mut self, older: bool) -> Event {
        if older {
            if self.selected == self.history_len { return Event::None; }
            if self.selected == 0 {
                self.draft = Command { bytes: self.bytes, len: self.len };
                self.draft_rejected = self.rejected;
            }
            self.selected += 1;
        } else {
            if self.selected == 0 { return Event::None; }
            self.selected -= 1;
        }
        let command = if self.selected == 0 {
            self.draft
        } else {
            self.history[(self.history_head + HISTORY_COMMANDS - self.selected) % HISTORY_COMMANDS]
        };
        self.bytes = command.bytes;
        self.len = command.len.min(self.limit);
        self.rejected = if self.selected == 0 { self.draft_rejected } else { command.len > self.limit };
        Event::Redraw
    }

    fn escape_byte(&mut self, byte: u8) -> Event {
        if self.escape == 1 {
            self.escape = if matches!(byte, b'[' | b'O') { 2 } else { 0 };
            self.params_len = 0;
            return Event::None;
        }
        if (0x20..=0x3f).contains(&byte) {
            if self.params_len < self.params.len() {
                self.params[self.params_len] = byte;
                self.params_len += 1;
            } else {
                self.escape = 3; // discard an oversized sequence through its final byte
            }
            return Event::None;
        }
        let valid = self.escape == 2;
        self.escape = 0;
        let params = &self.params[..self.params_len];
        if !valid { return Event::None; }
        match (byte, params) {
            (b'A', b"" | b"1") => self.recall(true),
            (b'B', b"" | b"1") => self.recall(false),
            (b'A', b"1;6") | (b'~', b"5" | b"5;2") => Event::PageUp,
            (b'B', b"1;6") | (b'~', b"6" | b"6;2") => Event::PageDown,
            _ => Event::None,
        }
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
                self.remember();
                Event::Submit
            };
        }
        if byte == 27 {
            self.escape = 1;
            return Event::None;
        }
        if self.escape != 0 {
            return self.escape_byte(byte);
        }
        if byte == 8 || byte == 127 {
            if self.len == 0 {
                return Event::None;
            }
            self.len -= 1;
            self.selected = 0;
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
        self.selected = 0;
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

/// Prints file contents in bounded UTF-8 chunks, with terminal controls filtered.
pub fn write_file_text(text: &str, out: &mut impl Write) -> fmt::Result {
    let mut chunk = FixedBuf::<512>::new();
    for c in text.chars() {
        let c = if c.is_control() && c != '\n' && c != '\t' { '?' } else { c };
        if chunk.as_str().len() + c.len_utf8() > 512 {
            out.write_str(chunk.as_str())?;
            chunk = FixedBuf::new();
        }
        chunk.write_char(c)?;
    }
    if !chunk.as_str().is_empty() {
        out.write_str(chunk.as_str())?;
    }
    if !text.ends_with('\n') {
        out.write_char('\n')?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receive_sequence(line: &mut LineEditor, sequence: &[u8]) -> Event {
        sequence.iter().fold(Event::None, |_, &byte| line.receive(byte))
    }

    fn submit(line: &mut LineEditor, text: &str) {
        for byte in text.bytes() { line.receive(byte); }
        assert_eq!(line.receive(b'\n'), Event::Submit);
        line.clear();
    }

    #[test]
    fn history_recalls_commands_and_restores_the_unsubmitted_draft() {
        let mut line = LineEditor::new(MAX_LINE);
        submit(&mut line, "help");
        submit(&mut line, "cat /etc/motd");
        receive_sequence(&mut line, b"draft");
        assert_eq!(receive_sequence(&mut line, b"\x1b[A"), Event::Redraw);
        assert_eq!(line.text(), "cat /etc/motd");
        assert_eq!(receive_sequence(&mut line, b"\x1bOA"), Event::Redraw);
        assert_eq!(line.text(), "help");
        assert_eq!(receive_sequence(&mut line, b"\x1b[A"), Event::None);
        receive_sequence(&mut line, b"\x1b[B\x1bOB");
        assert_eq!(line.text(), "draft");
        assert_eq!(receive_sequence(&mut line, b"\x1b[B"), Event::None);
        receive_sequence(&mut line, b"\x1b[A");
        assert_eq!(line.receive(b'\n'), Event::Submit, "recall is executed only by Enter");
    }

    #[test]
    fn history_is_bounded_deduplicated_and_editing_does_not_change_saved_commands() {
        let mut line = LineEditor::new(MAX_LINE);
        for i in 0..HISTORY_COMMANDS + 5 {
            submit(&mut line, &alloc::format!("cmd{i}"));
        }
        let count = line.history_len;
        submit(&mut line, &alloc::format!("cmd{}", HISTORY_COMMANDS + 4));
        submit(&mut line, "   ");
        assert_eq!(line.history_len, count);
        for _ in 0..HISTORY_COMMANDS + 5 { receive_sequence(&mut line, b"\x1b[A"); }
        assert_eq!(line.text(), "cmd5");
        line.receive(b'x');
        assert_eq!(line.text(), "cmd5x");
        receive_sequence(&mut line, b"\x1b[A\x1b[B");
        assert_eq!(line.text(), "cmd5x");
        for _ in 0..HISTORY_COMMANDS { receive_sequence(&mut line, b"\x1b[A"); }
        assert_eq!(line.text(), "cmd5", "saved entry is immutable while editing");
    }

    #[test]
    fn rejected_drafts_stay_rejected_after_history_browsing_and_escape_input_is_bounded() {
        let mut line = LineEditor::new(3);
        submit(&mut line, "ok");
        receive_sequence(&mut line, b"abcd");
        assert_eq!(line.receive(b'\n'), Event::Rejected);
        receive_sequence(&mut line, b"\x1b[A\x1b[B");
        assert_eq!(line.text(), "abc");
        assert_eq!(line.receive(b'\n'), Event::Rejected);
        assert_eq!(line.history_len, 1);
        line.clear();
        assert_eq!(receive_sequence(&mut line, b"\x1b[5~"), Event::PageUp);
        assert_eq!(receive_sequence(&mut line, b"\x1b[6;2~"), Event::PageDown);
        assert_eq!(receive_sequence(&mut line, b"\x1b[1;6A"), Event::PageUp);
        assert_eq!(receive_sequence(&mut line, b"\x1b[1;6B"), Event::PageDown);
        receive_sequence(&mut line, b"\x1b[123456789012345678901234567890A");
        assert_eq!(line.text(), "");
        line.receive(27);
        assert!(line.escape_pending());
        line.cancel_escape();
        assert!(!line.escape_pending());
        assert_eq!(line.receive(b'a'), Event::Echo(b'a'));
    }

    #[test]
    fn file_text_filters_controls_batches_output_and_keeps_utf8_boundaries() {
        struct Output { text: String, calls: usize }
        impl Write for Output {
            fn write_str(&mut self, s: &str) -> fmt::Result {
                assert!(s.len() <= 512);
                self.calls += 1;
                self.text.push_str(s);
                Ok(())
            }
        }
        let text = "a".repeat(511) + "Ж😀\x1b[31m\r\0\x08\u{85}\t\n" + &"z".repeat(2000);
        let expected: String = text.chars()
            .map(|c| if c.is_control() && c != '\n' && c != '\t' { '?' } else { c })
            .chain(['\n']).collect();
        let mut out = Output { text: String::new(), calls: 0 };
        write_file_text(&text, &mut out).unwrap();
        assert_eq!(out.text, expected);
        assert!(out.calls < 10, "file output must be batched");
        for (input, expected) in [("", "\n"), ("x\n", "x\n"), ("x\r", "x?\n")] {
            let mut out = String::new();
            write_file_text(input, &mut out).unwrap();
            assert_eq!(out, expected);
        }
    }

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
