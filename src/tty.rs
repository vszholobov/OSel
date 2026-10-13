//! Shared `/dev/tty`: UTF-8 output, serial/PS2 input and scheduler-backed reads.

use crate::sync::IrqSpinlock;
use crate::{keyboard, scheduler};
use alloc::vec::Vec;
use core::fmt::Write;
use osel_core::errno::{Errno, KResult};
use osel_core::exception::FixedBuf;
use osel_core::file::{FileLike, Ready};
use osel_core::keyboard::RingBuffer;
use osel_core::sched::ThreadId;
use osel_core::syscall::Utf8Decoder;
use osel_core::terminal::{Input, TERMIOS_SIZE, Termios};
use osel_core::vfs::Stat;
use x86_64::instructions::interrupts as irq;

static TEXT: IrqSpinlock<Utf8Decoder> = IrqSpinlock::new("TTY_TEXT", Utf8Decoder::new());
static INPUT: IrqSpinlock<Input> = IrqSpinlock::new("TTY_INPUT", Input::new());
static SERIAL_INPUT: IrqSpinlock<RingBuffer<u8, 1024>> =
    IrqSpinlock::new("TTY_SERIAL_INPUT", RingBuffer::new(0));
static WAITERS: IrqSpinlock<Vec<ThreadId>> = IrqSpinlock::new("TTY_WAITERS", Vec::new());
static OUTPUT: IrqSpinlock<Output> = IrqSpinlock::new(
    "TTY_OUTPUT",
    Output {
        busy: false,
        waiters: Vec::new(),
    },
);

struct Output {
    busy: bool,
    waiters: Vec<ThreadId>,
}

/// Serializes user writes without masking IRQs while drawing or waiting for COM1.
/// Contending writers sleep; no spinlock is carried across a context switch.
struct OutputGuard;

impl OutputGuard {
    fn acquire() -> KResult<Self> {
        let enabled = irq::are_enabled();
        loop {
            irq::disable();
            if scheduler::current_killed() {
                if enabled {
                    irq::enable();
                }
                return Err(Errno::EINTR);
            }
            let mut output = OUTPUT.lock();
            if !output.busy {
                output.busy = true;
                drop(output);
                if enabled {
                    irq::enable();
                }
                return Ok(Self);
            }
            let id = scheduler::current_id();
            output.waiters.push(id);
            drop(output);
            scheduler::block_current();
            OUTPUT.lock().waiters.retain(|&waiter| waiter != id);
            if enabled {
                irq::enable();
            }
        }
    }
}

impl Drop for OutputGuard {
    fn drop(&mut self) {
        irq::without_interrupts(|| {
            let mut output = OUTPUT.lock();
            output.busy = false;
            for id in output.waiters.drain(..) {
                scheduler::wake(id);
            }
        });
    }
}

pub fn init(max_waiters: usize) {
    WAITERS.lock().reserve(max_waiters);
    OUTPUT.lock().waiters.reserve(max_waiters);
}

pub fn forget(thread: ThreadId) {
    WAITERS.lock().retain(|&id| id != thread);
    OUTPUT.lock().waiters.retain(|&id| id != thread);
}

/// Called by IRQ handlers, without allocation and with interrupts disabled.
pub fn notify_input() {
    // Drop the queue lock before taking the scheduler lock. IRQs keep the queue stable.
    let count = WAITERS.lock().len();
    for index in 0..count {
        let id = WAITERS.lock()[index];
        scheduler::wake(id);
    }
}

/// A bounded COM1 poll on every PIT tick. UART IRQ4 stays masked; input latency is <=10ms.
pub fn poll_serial() {
    let mut bytes = [0; 64];
    let count = crate::cereal::receive(&mut bytes);
    if count == 0 {
        return;
    }
    {
        let mut input = SERIAL_INPUT.lock();
        for &byte in &bytes[..count] {
            input.push(byte);
        }
    }
    notify_input();
}

pub fn get_termios() -> [u8; TERMIOS_SIZE] {
    INPUT.lock().settings.0
}

pub fn set_termios(bytes: &[u8; TERMIOS_SIZE], flush: bool) -> KResult<()> {
    irq::without_interrupts(|| {
        INPUT.lock().configure(Termios(*bytes), flush)?;
        if flush {
            SERIAL_INPUT.lock().clear();
            while keyboard::pop_char().is_some() {}
        }
        notify_input();
        Ok(())
    })
}

pub fn winsize() -> [u8; 8] {
    let (rows, cols, width, height) = crate::avganec::with_console(|console| {
        (
            console.rows(),
            console.columns(),
            console.info().width,
            console.info().height,
        )
    })
    .unwrap_or((25, 80, 0, 0));
    let mut out = [0; 8];
    for (i, value) in [rows, cols, width, height].into_iter().enumerate() {
        out[i * 2..i * 2 + 2].copy_from_slice(&(value.min(u16::MAX as usize) as u16).to_le_bytes());
    }
    out
}

pub struct Tty {
    stat: Stat,
}

impl Tty {
    pub fn new(stat: Stat) -> Self {
        Self { stat }
    }
}

fn output(buf: &[u8]) -> KResult<()> {
    output_with(buf, |text| {
        for c in text.chars() {
            crate::avganec::with_console(|console| console.write_char(c));
        }
        crate::cereal::write_terminal(text.as_bytes())
    })
}

fn output_with(buf: &[u8], mut emit: impl FnMut(&str) -> KResult<()>) -> KResult<()> {
    let _writer = OutputGuard::acquire()?;
    for chunk in buf.chunks(64) {
        // Invalid UTF-8 can expand to three bytes per input byte, plus a pending
        // replacement from the previous chunk. Never allocate with IRQs masked.
        let mut text = FixedBuf::<196>::new();
        TEXT.lock().decode(chunk, |piece| {
            let _ = text.write_str(piece);
        });
        debug_assert!(!text.truncated());
        emit(text.as_str())?;
    }
    Ok(())
}

/// Decode and echo in process context, never from an interrupt handler.
fn collect_input() {
    let receive = |byte| {
        let (echo, count) = INPUT.lock().receive(byte);
        if count != 0 {
            let _ = output(&echo[..count]);
        }
    };
    loop {
        let byte = SERIAL_INPUT.lock().pop();
        let Some(byte) = byte else { break };
        receive(byte);
    }
    while let Some(c) = keyboard::pop_char() {
        let mut bytes = [0; 4];
        for &byte in c.encode_utf8(&mut bytes).as_bytes() {
            receive(byte);
        }
    }
}

impl FileLike for Tty {
    fn read(&self, _offset: u64, buf: &mut [u8], nonblock: bool) -> KResult<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        loop {
            // Check, register and block atomically against keyboard and timer interrupts.
            irq::disable();
            if scheduler::current_killed() {
                irq::enable();
                return Err(Errno::EINTR);
            }
            collect_input();
            let result = {
                let mut input = INPUT.lock();
                if input.available(buf.len())
                    || (nonblock && !input.settings.canonical() && input.has_bytes())
                {
                    Some(Ok(input.read(buf)))
                } else if nonblock {
                    Some(Err(Errno::EAGAIN))
                } else {
                    None
                }
            };
            if let Some(result) = result {
                irq::enable();
                return result;
            }
            let id = scheduler::current_id();
            WAITERS.lock().push(id);
            scheduler::block_current();
            forget(id);
            irq::enable();
        }
    }

    fn write(&self, _offset: u64, buf: &[u8]) -> KResult<usize> {
        output(buf)?;
        Ok(buf.len())
    }

    fn stat(&self) -> Stat {
        self.stat
    }

    fn poll(&self) -> Ready {
        Ready {
            readable: INPUT.lock().available(1),
            writable: true,
        }
    }
}

pub fn output_pending() -> bool {
    TEXT.lock().is_pending()
}

#[cfg(test)]
mod tests {
    use super::*;
    use core::sync::atomic::{AtomicU8, Ordering};

    static READ_BYTE: AtomicU8 = AtomicU8::new(0);

    fn competing_writer() {
        let _guard = OutputGuard::acquire().unwrap();
        assert!(irq::are_enabled());
        READ_BYTE.store(1, Ordering::SeqCst);
    }

    #[test_case]
    fn test_terminal_writers_sleep_without_masking_timer() {
        READ_BYTE.store(0, Ordering::SeqCst);
        let guard = OutputGuard::acquire().unwrap();
        assert!(irq::are_enabled());
        let id = scheduler::spawn_kernel(competing_writer).unwrap();
        wait_for(|| OUTPUT.lock().waiters.contains(&id));
        let runs = scheduler::runs(id);
        let tick = crate::interrupts::ticks();
        wait_for(|| crate::interrupts::ticks() >= tick + 3);
        assert_eq!(scheduler::runs(id), runs, "writer must sleep, not spin");
        assert_eq!(READ_BYTE.load(Ordering::SeqCst), 0);
        drop(guard);
        wait_for(|| scheduler::runs(id).is_none());
        assert_eq!(READ_BYTE.load(Ordering::SeqCst), 1);
        assert!(!OUTPUT.lock().busy);
        assert!(OUTPUT.lock().waiters.is_empty());
    }

    #[test_case]
    fn test_terminal_output_is_bounded_preemptible_and_preserves_utf8() {
        let mut output = alloc::string::String::new();
        let mut collect = |text: &str| {
            assert!(
                irq::are_enabled(),
                "drawing must not mask IRQs for the whole write"
            );
            assert!(text.len() <= 196);
            output.push_str(text);
            Ok(())
        };
        output_with(&[0xff; 130], &mut collect).unwrap();
        output_with(&[0xd0], &mut collect).unwrap();
        output_with(&[0x96], &mut collect).unwrap();
        assert_eq!(output, "�".repeat(130) + "Ж");
        assert!(!output_pending());
        assert!(!OUTPUT.lock().busy);
    }

    #[test_case]
    fn test_terminal_output_error_releases_writer() {
        assert_eq!(output_with(b"x", |_| Err(Errno::EINTR)), Err(Errno::EINTR));
        assert!(!OUTPUT.lock().busy);
        output_with(b"y", |_| Ok(())).unwrap();
    }

    fn reader() {
        let tty = Tty::new(Stat::default());
        let mut byte = [0];
        assert_eq!(tty.read(0, &mut byte, false), Ok(1));
        READ_BYTE.store(byte[0], Ordering::SeqCst);
    }

    fn wait_for(mut done: impl FnMut() -> bool) {
        let start = crate::interrupts::ticks();
        while !done() {
            assert!(
                crate::interrupts::ticks() < start + 100,
                "terminal test timed out"
            );
            scheduler::yield_now();
        }
    }

    #[test_case]
    fn test_terminal_read_blocks_without_spinning_and_wakes_on_keyboard() {
        let old = get_termios();
        set_termios(&Termios::new().0, true).unwrap();
        READ_BYTE.store(0, Ordering::SeqCst);
        let id = scheduler::spawn_kernel(reader).unwrap();
        wait_for(|| WAITERS.lock().contains(&id));
        let runs = scheduler::runs(id);
        let tick = crate::interrupts::ticks();
        wait_for(|| crate::interrupts::ticks() >= tick + 3);
        assert_eq!(
            scheduler::runs(id),
            runs,
            "blocked reader must leave the run queue"
        );
        irq::without_interrupts(|| {
            keyboard::handle_scancode(0x1e);
            keyboard::handle_scancode(0x9e);
        });
        wait_for(|| scheduler::runs(id).is_none());
        assert_eq!(READ_BYTE.load(Ordering::SeqCst), b'a');
        assert!(WAITERS.lock().is_empty());
        set_termios(&old, true).unwrap();
    }

    #[test_case]
    fn test_terminal_serial_bytes_nonblock_and_settings_roundtrip() {
        let old = get_termios();
        set_termios(&Termios::new().0, true).unwrap();
        let tty = Tty::new(Stat::default());
        let mut buf = [0; 3];
        assert_eq!(tty.read(0, &mut buf, true), Err(Errno::EAGAIN));
        irq::without_interrupts(|| {
            for byte in "Жa".bytes() {
                SERIAL_INPUT.lock().push(byte);
            }
            notify_input();
        });
        assert_eq!(tty.read(0, &mut buf[..1], true), Ok(1));
        assert_eq!(buf[0], 0xd0);
        assert_eq!(tty.read(0, &mut buf, true), Ok(2));
        assert_eq!(&buf[..2], &[0x96, b'a']);
        let mut mode = get_termios();
        mode[12] = osel_core::terminal::ICANON as u8;
        set_termios(&mode, false).unwrap();
        assert_eq!(get_termios(), mode);
        assert!(u16::from_le_bytes(winsize()[..2].try_into().unwrap()) > 0);
        set_termios(&old, true).unwrap();
    }
}
