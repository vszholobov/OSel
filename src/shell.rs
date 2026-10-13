//! Minimal foreground shell using the existing VFS, TTY and program loader.

use crate::{fs, program, tty, user::Stop};
use alloc::vec::Vec;
use core::fmt::{self, Write};
use fs::{Errno, FileType, KResult};
use osel_core::file::FileLike;
use osel_core::shell::{self, Event, LineEditor, MAX_LINE};
use osel_core::terminal::Termios;

const PROMPT: &str = "osel> ";
const ENV: &[&[u8]] = &[b"PATH=/bin", b"TERM=xterm", b"GODEBUG=asyncpreemptoff=1"];
const MAX_CAT_BYTES: u64 = 64 * 1024;

struct Output<'a>(&'a dyn FileLike);
impl Write for Output<'_> {
    fn write_str(&mut self, text: &str) -> fmt::Result {
        self.0
            .write(0, text.as_bytes())
            .map(|_| ())
            .map_err(|_| fmt::Error)
    }
}

pub fn run() -> ! {
    let terminal = tty::Tty::new(osel_core::vfs::Stat::default());
    let mut out = Output(&terminal);
    let _ = tty::set_termios(&Termios::new().0, true);
    let columns = crate::avganec::with_console(|c| c.columns()).unwrap_or(80);
    // Keep the editable line on one row; framebuffer backspace cannot cross rows.
    let mut line = LineEditor::new(columns.saturating_sub(PROMPT.len() + 1).min(MAX_LINE));
    let _ = writeln!(
        out,
        "OSel shell. Type help. Ctrl+C cancels the current line."
    );
    let _ = write!(out, "{PROMPT}");
    loop {
        let mut byte = [0];
        if terminal.read(0, &mut byte, false) != Ok(1) {
            continue;
        }
        match line.receive(byte[0]) {
            Event::None => {}
            Event::Echo(b) => {
                let _ = write!(out, "{}", b as char);
            }
            Event::Erase => {
                let _ = out.write_str("\x08 \x08");
            }
            Event::Cancel => {
                let _ = write!(out, "^C\n{PROMPT}");
            }
            event @ (Event::Submit | Event::Rejected) => {
                let _ = out.write_char('\n');
                if event == Event::Rejected {
                    let _ = writeln!(
                        out,
                        "shell: line too long or non-ASCII input; command not executed"
                    );
                } else {
                    execute(line.text(), &mut out);
                }
                line.clear();
                let _ = write!(out, "{PROMPT}");
            }
        }
    }
}

/// Returns a shell status, while diagnostics go to the supplied writer.
pub fn execute(line: &str, out: &mut impl Write) -> u8 {
    let args = match shell::parse(line) {
        Ok(args) => args,
        Err(error) => {
            let _ = writeln!(out, "shell: {error:?}");
            return 2;
        }
    };
    let Some(&command) = args.first() else {
        return 0;
    };
    let result = match command {
        "help" if args.len() == 1 => {
            let _ = out.write_str("help             commands and limitations\nls [path]        list a directory (default /)\ncat path         print a file (up to 64 KiB)\ntetris           play; x returns to the shell\nclear            run /bin/clear\nhello, gohello   example programs\n/bin/name args   run an executable; bare names use /bin\nPaths are relative to /. No quotes, pipes, redirects or background jobs.\n");
            Ok(0)
        }
        "ls" if args.len() <= 2 => list(args.get(1).copied().unwrap_or("/"), out),
        "cat" if args.len() == 2 => cat(args[1], out),
        "help" | "ls" | "cat" => Err(Errno::EINVAL),
        _ => external(&args, out),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            let _ = writeln!(out, "{command}: {error}");
            1
        }
    }
}

fn list(path: &str, out: &mut impl Write) -> KResult<u8> {
    let inode = fs::lookup(path)?;
    let mut cursor = 0;
    while let Some((entry, next)) = inode.read_dir(cursor)? {
        let suffix = if entry.file_type == FileType::Directory {
            "/"
        } else {
            ""
        };
        let _ = writeln!(out, "{}{suffix}", entry.name);
        cursor = next;
    }
    Ok(0)
}

fn cat(path: &str, out: &mut impl Write) -> KResult<u8> {
    let inode = fs::lookup(path)?;
    let stat = inode.stat();
    if stat.file_type != FileType::Regular {
        return Err(Errno::EINVAL);
    }
    if stat.size > MAX_CAT_BYTES {
        return Err(Errno::E2BIG);
    }
    let bytes = fs::read_to_end(&*inode)?;
    let text = alloc::string::String::from_utf8_lossy(&bytes);
    // Display control bytes visibly instead of letting file contents control the terminal.
    for c in text.chars() {
        let _ = out.write_char(if c.is_control() && c != '\n' && c != '\t' {
            '?'
        } else {
            c
        });
    }
    if !text.ends_with('\n') {
        let _ = out.write_char('\n');
    }
    Ok(0)
}

fn external(args: &[&str], out: &mut impl Write) -> KResult<u8> {
    let path = shell::executable(args[0]);
    let stat = fs::lookup(&path)?.stat();
    if stat.file_type != FileType::Regular || stat.permissions & 0o111 == 0 {
        return Err(Errno::EACCES);
    }
    let mut argv: Vec<&[u8]> = args.iter().map(|s| s.as_bytes()).collect();
    if path.rsplit('/').next() == Some("tetris") && !args.contains(&"--single") {
        argv.insert(1, b"--single");
    }
    let settings = tty::get_termios();
    let result = program::run(&path, &argv, ENV);
    // A failed/crashed program must not leave the shell in canonical/no-echo mode.
    tty::set_termios(&settings, true)?;
    let _ = out.write_str("\x1b[0m\x1b[?25h");
    match result? {
        Stop::Exit(code) => {
            if code != 0 {
                let _ = writeln!(out, "{path}: exit {code}");
            }
            Ok(code)
        }
        stop => {
            let _ = writeln!(out, "{path}: {stop:?}");
            Ok(128)
        }
    }
}
