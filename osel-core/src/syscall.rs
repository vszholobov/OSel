//! Linux system calls: numbers, arguments, results and the way back to the program
//!
//! The kernel decides what each call does; here is what is the same for all of them.
//! `docs/reference/syscalls.md` lists every call of `SYSCALLS`, `cargo docs-check` checks it.

use crate::errno::KResult;
use crate::trap::{GENERAL_PROTECTION, TrapFrame};

pub const READ: u64 = 0;
pub const WRITE: u64 = 1;
pub const CLOSE: u64 = 3;
pub const FSTAT: u64 = 5;
pub const MMAP: u64 = 9;
pub const MPROTECT: u64 = 10;
pub const MUNMAP: u64 = 11;
pub const SCHED_YIELD: u64 = 24;
pub const MADVISE: u64 = 28;
pub const RT_SIGACTION: u64 = 13;
pub const RT_SIGPROCMASK: u64 = 14;
pub const NANOSLEEP: u64 = 35;
pub const CLONE: u64 = 56;
pub const SIGALTSTACK: u64 = 131;
pub const ARCH_PRCTL: u64 = 158;
pub const GETTID: u64 = 186;
pub const SCHED_GETAFFINITY: u64 = 204;
pub const TGKILL: u64 = 234;
pub const PRCTL: u64 = 157;
pub const PRLIMIT64: u64 = 302;
pub const GETRANDOM: u64 = 318;
pub const FUTEX: u64 = 202;
pub const EPOLL_CTL: u64 = 233;
pub const EPOLL_PWAIT: u64 = 281;
pub const EVENTFD2: u64 = 290;
pub const EPOLL_CREATE1: u64 = 291;
pub const CLOCK_GETTIME: u64 = 228;
pub const GETPID: u64 = 39;
pub const EXIT: u64 = 60;
pub const FCNTL: u64 = 72;
pub const EXIT_GROUP: u64 = 231;
pub const OPENAT: u64 = 257;
pub const NEWFSTATAT: u64 = 262;

/// Every system call OSel implements: Linux x86_64 number and name
pub const SYSCALLS: &[(u64, &str)] = &[
    (READ, "read"),
    (WRITE, "write"),
    (CLOSE, "close"),
    (FSTAT, "fstat"),
    (MMAP, "mmap"),
    (MPROTECT, "mprotect"),
    (MUNMAP, "munmap"),
    (SCHED_YIELD, "sched_yield"),
    (MADVISE, "madvise"),
    (RT_SIGACTION, "rt_sigaction"),
    (RT_SIGPROCMASK, "rt_sigprocmask"),
    (NANOSLEEP, "nanosleep"),
    (CLONE, "clone"),
    (SIGALTSTACK, "sigaltstack"),
    (PRCTL, "prctl"),
    (ARCH_PRCTL, "arch_prctl"),
    (GETTID, "gettid"),
    (SCHED_GETAFFINITY, "sched_getaffinity"),
    (TGKILL, "tgkill"),
    (PRLIMIT64, "prlimit64"),
    (GETRANDOM, "getrandom"),
    (FUTEX, "futex"),
    (EPOLL_CTL, "epoll_ctl"),
    (EPOLL_PWAIT, "epoll_pwait"),
    (EVENTFD2, "eventfd2"),
    (EPOLL_CREATE1, "epoll_create1"),
    (CLOCK_GETTIME, "clock_gettime"),
    (GETPID, "getpid"),
    (EXIT, "exit"),
    (FCNTL, "fcntl"),
    (EXIT_GROUP, "exit_group"),
    (OPENAT, "openat"),
    (NEWFSTATAT, "newfstatat"),
];

/// A descriptor argument: Linux takes it as a 32-bit `int`, the upper half of the register
/// is whatever the program left there (Go loads `AT_FDCWD` with a 32-bit move)
pub fn fd_arg(value: u64) -> i32 {
    value as u32 as i32
}

/// `TrapFrame::vector` of an entry through `syscall`: not a CPU vector
pub const SYSCALL_VECTOR: u64 = 256;

/// RFLAGS: trap flag
const RFLAGS_TF: u64 = 1 << 8;

/// The call number: like Linux, only the low 32 bits count, as a signed `int`.
/// `None` for a negative number, which no call has.
pub fn number(frame: &TrapFrame) -> Option<u64> {
    let nr = frame.rax as u32 as i32;
    (nr >= 0).then_some(nr as u64)
}

/// Name of the call `nr`, if OSel implements it
pub fn name(nr: u64) -> Option<&'static str> {
    SYSCALLS.iter().find(|(number, _)| *number == nr).map(|(_, name)| *name)
}

/// The six arguments in the order of the Linux system call ABI
pub fn args(frame: &TrapFrame) -> [u64; 6] {
    [frame.rdi, frame.rsi, frame.rdx, frame.r10, frame.r8, frame.r9]
}

/// What goes back into RAX: the value, or `-errno`
pub fn encode(result: KResult<u64>) -> u64 {
    match result {
        Ok(value) => value,
        Err(errno) => (-(errno.code() as i64)) as u64,
    }
}

/// How the kernel goes back to the program after a call
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u64)]
pub enum Return {
    /// `sysretq`: the fast way
    Sysret = 0,
    /// The run ends: the program exited, or can't be returned to
    Leave = 1,
    /// `iretq`: needed when the trap flag is set, `sysretq` would trap one instruction early
    Iret = 2,
}

/// Prepares `frame` for the way back and picks it. A return address outside the lower half
/// ends the program with #GP: `sysretq` there faults in ring 0 on Intel, `iretq` faults
/// before leaving ring 0 — both would crash the kernel.
pub fn return_path(frame: &mut TrapFrame, code_selector: u16, data_selector: u16, user_end: u64) -> Return {
    if !frame.make_user(code_selector, data_selector, user_end) {
        frame.vector = GENERAL_PROTECTION;
        frame.error_code = 0;
        return Return::Leave;
    }
    if frame.rflags & RFLAGS_TF != 0 { Return::Iret } else { Return::Sysret }
}

/// Turns bytes a program writes into text, whatever chunks they arrive in: a character
/// split between two chunks is kept until it is complete, broken bytes become U+FFFD
#[derive(Debug, Default)]
pub struct Utf8Decoder {
    pending: [u8; 4],
    pending_len: usize,
}

impl Utf8Decoder {
    pub const fn new() -> Utf8Decoder {
        Utf8Decoder { pending: [0; 4], pending_len: 0 }
    }

    /// Whether the bytes so far end in the middle of a character
    pub fn is_pending(&self) -> bool {
        self.pending_len > 0
    }

    /// Hands the text of `bytes` to `output`, piece by piece
    pub fn decode(&mut self, bytes: &[u8], mut output: impl FnMut(&str)) {
        self.decode_into(bytes, &mut output);
    }

    fn decode_into(&mut self, mut bytes: &[u8], output: &mut dyn FnMut(&str)) {
        while self.pending_len > 0 && !bytes.is_empty() {
            self.pending[self.pending_len] = bytes[0];
            self.pending_len += 1;
            bytes = &bytes[1..];
            match core::str::from_utf8(&self.pending[..self.pending_len]) {
                Ok(c) => {
                    output(c);
                    self.pending_len = 0;
                }
                // still not complete
                Err(e) if e.error_len().is_none() => {}
                Err(e) => {
                    // the pending bytes were not a character: one U+FFFD for the broken
                    // part, like `String::from_utf8_lossy`, then the rest again
                    let bad = e.error_len().unwrap_or(1);
                    let rest = self.pending_len - bad;
                    let mut retry = [0u8; 4];
                    retry[..rest].copy_from_slice(&self.pending[bad..self.pending_len]);
                    self.pending_len = 0;
                    output("\u{fffd}");
                    self.decode_into(&retry[..rest], output);
                }
            }
        }
        while !bytes.is_empty() {
            match core::str::from_utf8(bytes) {
                Ok(text) => {
                    output(text);
                    return;
                }
                Err(e) => {
                    let valid = e.valid_up_to();
                    // SAFETY: `from_utf8` checked these bytes
                    output(unsafe { core::str::from_utf8_unchecked(&bytes[..valid]) });
                    match e.error_len() {
                        Some(bad) => {
                            output("\u{fffd}");
                            bytes = &bytes[valid + bad..];
                        }
                        None => {
                            let rest = &bytes[valid..];
                            self.pending[..rest.len()].copy_from_slice(rest);
                            self.pending_len = rest.len();
                            return;
                        }
                    }
                }
            }
        }
    }

    /// The end of the bytes: an unfinished character becomes U+FFFD
    pub fn finish(&mut self, mut output: impl FnMut(&str)) {
        if self.pending_len > 0 {
            self.pending_len = 0;
            output("\u{fffd}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errno::Errno;
    use crate::trap::{RFLAGS_IF, RFLAGS_RESERVED};

    const USER_END: u64 = 1 << 47;

    #[test]
    fn numbers_are_32_bit_like_linux() {
        let frame = |rax| TrapFrame { rax, ..TrapFrame::default() };
        assert_eq!(number(&frame(1)), Some(1));
        assert_eq!(number(&frame(0x1_0000_0001)), Some(1), "the high half is ignored");
        assert_eq!(number(&frame(u64::MAX)), None, "-1");
        assert_eq!(number(&frame(0x8000_0000)), None, "negative as an int");
        assert_eq!(number(&frame(9999)), Some(9999));
    }

    #[test]
    fn the_table_has_unique_numbers_and_names() {
        for (i, (nr, name)) in SYSCALLS.iter().enumerate() {
            assert!(SYSCALLS[i + 1..].iter().all(|(other, other_name)| other != nr && other_name != name), "{name}");
            assert_eq!(super::name(*nr), Some(*name));
        }
        assert_eq!(super::name(9999), None);
    }

    #[test]
    fn descriptors_are_32_bit() {
        assert_eq!(fd_arg(3), 3);
        assert_eq!(fd_arg(0xffff_ff9c), -100, "AT_FDCWD as Go passes it");
        assert_eq!(fd_arg(0xffff_ffff_ffff_ff9c), -100, "and sign extended");
        assert_eq!(fd_arg(0xdead_0000_0000_0001), 1, "garbage above");
    }

    #[test]
    fn arguments_follow_the_linux_abi() {
        let frame = TrapFrame { rdi: 1, rsi: 2, rdx: 3, r10: 4, r8: 5, r9: 6, rcx: 99, ..TrapFrame::default() };
        assert_eq!(args(&frame), [1, 2, 3, 4, 5, 6], "r10, not rcx: syscall puts the return address there");
    }

    #[test]
    fn errors_are_negative() {
        assert_eq!(encode(Ok(5)), 5);
        assert_eq!(encode(Err(Errno::EFAULT)), (-14i64) as u64);
        assert_eq!(encode(Err(Errno::ENOSYS)), (-38i64) as u64);
    }

    fn decode_in_chunks(bytes: &[u8], chunk: usize) -> std::string::String {
        let mut text = std::string::String::new();
        let mut decoder = Utf8Decoder::default();
        for part in bytes.chunks(chunk) {
            decoder.decode(part, |s| text.push_str(s));
        }
        decoder.finish(|s| text.push_str(s));
        assert!(!decoder.is_pending());
        text
    }

    #[test]
    fn text_split_anywhere_comes_out_whole() {
        let text = "привет, 世界 🦀!";
        for chunk in 1..=text.len() {
            assert_eq!(decode_in_chunks(text.as_bytes(), chunk), text, "chunks of {chunk}");
        }
    }

    #[test]
    fn broken_bytes_become_replacement_characters() {
        for chunk in 1..=8 {
            assert_eq!(decode_in_chunks(b"a\xffb", chunk), "a\u{fffd}b", "{chunk}");
            assert_eq!(decode_in_chunks(b"a\xd0", chunk), "a\u{fffd}", "cut at the end, {chunk}");
            assert_eq!(decode_in_chunks(b"\xd0a", chunk), "\u{fffd}a", "a lead byte without its tail, {chunk}");
            assert_eq!(decode_in_chunks(b"\xe4\xb8x", chunk), "\u{fffd}x", "{chunk}");
        }
    }

    /// Whatever the chunks, the text is what `String::from_utf8_lossy` makes of all bytes
    #[test]
    fn random_bytes_match_from_utf8_lossy() {
        let mut seed = 0x5eed_u64;
        let mut random = move || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        let pieces: [&[u8]; 8] = [b"a", "я".as_bytes(), "世".as_bytes(), "🦀".as_bytes(), b"\xff", b"\xe4\xb8", b"\xf0\x9f", b"\x80"];
        for _ in 0..2000 {
            let bytes: std::vec::Vec<u8> = (0..random() % 12).flat_map(|_| pieces[(random() % 8) as usize].iter().copied()).collect();
            let expected = std::string::String::from_utf8_lossy(&bytes);
            for chunk in 1..=5 {
                assert_eq!(decode_in_chunks(&bytes, chunk), expected, "{bytes:x?} in chunks of {chunk}");
            }
        }
    }

    #[test]
    fn the_way_back() {
        let user = |rip, rflags| TrapFrame { rip, rflags, vector: SYSCALL_VECTOR, ..TrapFrame::default() };
        let mut frame = user(0x40_1000, RFLAGS_IF | RFLAGS_RESERVED | (3 << 12));
        assert_eq!(return_path(&mut frame, 0x23, 0x1b, USER_END), Return::Sysret);
        assert_eq!(frame.rflags & (3 << 12), 0, "no IOPL through sysret's R11");
        let mut traced = user(0x40_1000, RFLAGS_IF | RFLAGS_TF);
        assert_eq!(return_path(&mut traced, 0x23, 0x1b, USER_END), Return::Iret);
        // a syscall in the last two bytes of the lower half returns to USER_END
        let mut edge = user(USER_END, RFLAGS_IF);
        assert_eq!(return_path(&mut edge, 0x23, 0x1b, USER_END), Return::Leave);
        assert_eq!(edge.vector, GENERAL_PROTECTION);
        let mut last_byte = user(USER_END - 1, RFLAGS_IF);
        assert_eq!(return_path(&mut last_byte, 0x23, 0x1b, USER_END), Return::Sysret, "still canonical");
    }
}
