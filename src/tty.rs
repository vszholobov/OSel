//! tty module is the terminal a program reads and writes: the screen, the serial port and
//! the keyboard behind `/dev/tty` and the standard descriptors
//!
//! No line discipline yet: a read returns the keys typed so far without echo, a write
//! prints right away. Terminal modes come with stage 3.

use crate::keyboard;
use crate::sync::IrqSpinlock;
use alloc::string::String;
use osel_core::errno::{Errno, KResult};
use osel_core::file::FileLike;
use osel_core::syscall::Utf8Decoder;
use osel_core::vfs::Stat;

/// Output text state: a character may be split between two writes
static TEXT: IrqSpinlock<Utf8Decoder> = IrqSpinlock::new("TTY_TEXT", Utf8Decoder::new());

/// Bytes of a typed character that did not fit into the last read
static PENDING: IrqSpinlock<([u8; 4], usize)> = IrqSpinlock::new("TTY_PENDING", ([0; 4], 0));

/// The terminal; every open of it is the same device
pub struct Tty {
    stat: Stat,
}

impl Tty {
    /// `stat` is the device node's
    pub fn new(stat: Stat) -> Tty {
        Tty { stat }
    }
}

impl FileLike for Tty {
    /// Waits until a key is typed, then returns the typed characters as UTF-8; the bytes of a
    /// character that don't fit come with the next read. With `nonblock`, `EAGAIN` instead
    /// of waiting.
    fn read(&self, _offset: u64, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        let mut done = take_pending(buf);
        if done == 0 {
            let c = match keyboard::pop_char() {
                Some(c) => c,
                None if nonblock => return Err(Errno::EAGAIN),
                None => wait_for_key()?,
            };
            done = put_char(c, buf);
        }
        // whatever else was typed meanwhile
        while done < buf.len() {
            let Some(c) = keyboard::pop_char() else { break };
            done += put_char(c, &mut buf[done..]);
        }
        Ok(done)
    }

    fn write(&self, _offset: u64, buf: &[u8]) -> KResult<usize> {
        let mut text = String::new();
        // decoded under the lock, printed after it: printing takes the console locks
        TEXT.lock().decode(buf, |piece| text.push_str(piece));
        crate::print!("{}", text);
        crate::serial_print!("{}", text);
        Ok(buf.len())
    }

    fn stat(&self) -> Stat {
        self.stat
    }
}

/// Whether the last write ended in the middle of a character
pub fn output_pending() -> bool {
    TEXT.lock().is_pending()
}

/// The next typed key; sleeps in `hlt` until the keyboard interrupt brings one. Only from a
/// system call, which runs with interrupts on. `EINTR` once the process is killed: it must
/// not keep waiting, nor take keys meant for others.
fn wait_for_key() -> KResult<char> {
    use x86_64::instructions::interrupts;
    loop {
        if crate::scheduler::current_killed() {
            return Err(Errno::EINTR);
        }
        // checked with interrupts off: a key arriving right after the check must wake the hlt
        interrupts::disable();
        if let Some(c) = keyboard::pop_char() {
            interrupts::enable();
            return Ok(c);
        }
        interrupts::enable_and_hlt();
    }
}

/// Copies the bytes left over from the last read
fn take_pending(buf: &mut [u8]) -> usize {
    let mut pending = PENDING.lock();
    let (bytes, len) = &mut *pending;
    let count = (*len).min(buf.len());
    buf[..count].copy_from_slice(&bytes[..count]);
    bytes.copy_within(count..*len, 0);
    *len -= count;
    count
}

/// Puts `c` as UTF-8 into `buf`; what doesn't fit waits for the next read
fn put_char(c: char, buf: &mut [u8]) -> usize {
    let mut encoded = [0u8; 4];
    let bytes = c.encode_utf8(&mut encoded).as_bytes();
    let count = bytes.len().min(buf.len());
    buf[..count].copy_from_slice(&bytes[..count]);
    let mut pending = PENDING.lock();
    let rest = &bytes[count..];
    pending.0[..rest.len()].copy_from_slice(rest);
    pending.1 = rest.len();
    count
}
