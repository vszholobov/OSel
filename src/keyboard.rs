//! keyboard module decodes PS/2 scancodes and buffers typed characters

use crate::sync::IrqSpinlock;
use core::fmt::{self, Write};
use osel_core::keyboard::{RingBuffer, ScancodeDecoder};
use osel_core::ps2::{self, Io};
use x86_64::instructions::{interrupts, port::Port};

pub const BUFFER_SIZE: usize = 64;

static DECODER: IrqSpinlock<ScancodeDecoder> = IrqSpinlock::new("DECODER", ScancodeDecoder::new());

/// Typed characters, filled by the keyboard interrupt handler
static BUFFER: IrqSpinlock<RingBuffer<char, BUFFER_SIZE>> =
    IrqSpinlock::new("BUFFER", RingBuffer::new('\0'));

#[derive(Clone, Copy)]
pub struct Diagnostics {
    pub init: ps2::Report,
    pub irqs: u64,
    pub bytes: u64,
    pub polled: u64,
    pub chars: u64,
    pub rejected: u64,
    pub status: u8,
    pub last: u8,
    pending_ticks: u8,
}

static DIAGNOSTICS: IrqSpinlock<Diagnostics> = IrqSpinlock::new(
    "KEYBOARD_DIAGNOSTICS",
    Diagnostics {
        init: ps2::Report::new(),
        irqs: 0,
        bytes: 0,
        polled: 0,
        chars: 0,
        rejected: 0,
        status: 0,
        last: 0,
        pending_ticks: 0,
    },
);

struct Hardware;
impl Io for Hardware {
    fn status(&mut self) -> u8 {
        unsafe { Port::new(0x64).read() }
    }
    fn read(&mut self) -> u8 {
        unsafe { Port::new(0x60).read() }
    }
    fn command(&mut self, value: u8) {
        unsafe { Port::new(0x64).write(value) }
    }
    fn data(&mut self, value: u8) {
        unsafe { Port::new(0x60).write(value) }
    }
    fn delay(&mut self) {
        // Legacy I/O delay, not a CPU-speed-dependent spin and not dependent on PIT.
        // Port 0x80 is read only; the number of iterations bounds a broken EC wait.
        for _ in 0..32 {
            let _: u8 = unsafe { Port::new(0x80).read() };
        }
    }
}

/// Before PIC reset and enabling interrupts, with exclusive access to the controller.
pub fn init() {
    assert!(!interrupts::are_enabled());
    let report = ps2::initialize(&mut Hardware, 10_000);
    DIAGNOSTICS.lock().init = report;
}

pub fn diagnostics() -> Diagnostics {
    let mut snapshot = *DIAGNOSTICS.lock();
    snapshot.status = Hardware.status();
    snapshot
}

pub fn write_diagnostics(out: &mut impl Write) -> fmt::Result {
    let d = diagnostics();
    writeln!(
        out,
        "PS/2 init: {:?}, error={:?}",
        d.init.stage, d.init.error
    )?;
    writeln!(
        out,
        "PS/2 cfg={:02x?}->{:02x?} reply={:02x?} retries={} drained={}",
        d.init.config_before,
        d.init.config_after,
        d.init.last_reply,
        d.init.retries,
        d.init.discarded
    )?;
    write_counters(out)
}

pub fn write_counters(out: &mut impl Write) -> fmt::Result {
    let d = diagnostics();
    writeln!(
        out,
        "KBD t={} irq={} bytes={} poll={} chars={} bad={} st={:02x} last={:02x}",
        crate::interrupts::ticks(),
        d.irqs,
        d.bytes,
        d.polled,
        d.chars,
        d.rejected,
        d.status,
        d.last
    )
}

/// IRQ and fallback polling are serialized by disabled interrupts on our single CPU.
fn receive(polled: bool) {
    debug_assert!(!interrupts::are_enabled());
    for _ in 0..16 {
        let status = Hardware.status();
        if status == 0xff || status & ps2::OUTPUT_FULL == 0 {
            break;
        }
        let byte = Hardware.read();
        let accepted =
            ps2::is_scancode(status, byte) && DIAGNOSTICS.lock().init.stage == ps2::Stage::Ready;
        {
            let mut d = DIAGNOSTICS.lock();
            d.bytes += 1;
            d.polled += u64::from(polled);
            d.last = byte;
            d.rejected += u64::from(!accepted);
            d.pending_ticks = 0;
        }
        if accepted {
            handle_scancode(byte);
        }
    }
}

pub fn interrupt() {
    DIAGNOSTICS.lock().irqs += 1;
    receive(false);
}

/// Recover a byte stuck for two PIT ticks without IRQ1. Do not race a healthy IRQ
/// on the first tick; never wait, send commands, allocate or print in this path.
pub fn poll() {
    debug_assert!(!interrupts::are_enabled());
    let status = Hardware.status();
    let should_read = {
        let mut d = DIAGNOSTICS.lock();
        if status == 0xff || status & ps2::OUTPUT_FULL == 0 {
            d.pending_ticks = 0;
            false
        } else {
            d.pending_ticks = d.pending_ticks.saturating_add(1);
            d.pending_ticks >= 2
        }
    };
    if should_read {
        receive(true);
    }
}

/// Called from the keyboard interrupt handler with a raw scancode from port 0x60
pub fn handle_scancode(scancode: u8) {
    if let Some(c) = DECODER.lock().decode(scancode) {
        // when nobody reads the input it is dropped
        BUFFER.lock().push(c);
        DIAGNOSTICS.lock().chars += 1;
        crate::tty::notify_input();
    }
}

/// Takes the oldest typed character, if any
pub fn pop_char() -> Option<char> {
    BUFFER.lock().pop()
}

/// Number of characters lost because nobody read them in time
pub fn dropped_chars() -> usize {
    BUFFER.lock().dropped()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interrupts::ticks;
    use alloc::string::String;
    use x86_64::instructions::port::Port;

    const STATUS_OUTPUT_FULL: u8 = 0b01;
    const STATUS_INPUT_FULL: u8 = 0b10;
    /// About half a second at 100 Hz
    const TIMEOUT_TICKS: u64 = 50;

    /// Waits until `done` returns true, panics instead of hanging the test run
    fn wait_until(what: &str, mut done: impl FnMut() -> bool) {
        let start = ticks();
        while !done() {
            assert!(
                ticks() < start + TIMEOUT_TICKS,
                "timed out waiting for {}",
                what
            );
            x86_64::instructions::hlt();
        }
    }

    fn status() -> u8 {
        let mut command: Port<u8> = Port::new(0x64);
        unsafe { command.read() }
    }

    /// Makes the PS/2 controller deliver `scancode` as if it came from the keyboard
    fn inject_scancode(scancode: u8) {
        let mut command: Port<u8> = Port::new(0x64);
        let mut data: Port<u8> = Port::new(0x60);
        // the interrupt handler has to consume the previous byte first
        wait_until("keyboard interrupt to read the previous byte", || {
            status() & STATUS_OUTPUT_FULL == 0
        });
        wait_until("controller to accept a command", || {
            status() & STATUS_INPUT_FULL == 0
        });
        unsafe { command.write(0xd2) }; // write keyboard output buffer
        wait_until("controller to accept data", || {
            status() & STATUS_INPUT_FULL == 0
        });
        unsafe { data.write(scancode) };
    }

    fn inject_all(scancodes: &[u8]) {
        for &scancode in scancodes {
            inject_scancode(scancode);
        }
        wait_until("the last byte to be handled", || {
            status() & STATUS_OUTPUT_FULL == 0
        });
    }

    fn drain() -> String {
        core::iter::from_fn(pop_char).collect()
    }

    #[test_case]
    fn test_keyboard_interrupt() {
        drain();
        let before = diagnostics().irqs;
        inject_all(&[0x1e, 0x9e, 0x02, 0x82]);
        assert_eq!(drain(), "a1");
        assert!(diagnostics().irqs > before, "normal input must use IRQ1");
    }

    #[test_case]
    fn test_keyboard_initialization() {
        let report = diagnostics().init;
        assert_eq!(report.error, None);
        assert_eq!(report.stage, ps2::Stage::Ready);
        let config = report.config_after.unwrap();
        assert_eq!(
            config & (ps2::IRQ1 | ps2::TRANSLATE),
            ps2::IRQ1 | ps2::TRANSLATE
        );
        assert_eq!(config & (ps2::IRQ12 | ps2::KEYBOARD_DISABLED), 0);
    }

    #[test_case]
    fn test_keyboard_recovers_firmware_disabled_port_and_scanning() {
        drain();
        interrupts::without_interrupts(|| {
            fn ready() {
                for _ in 0..10_000 {
                    if Hardware.status() & ps2::INPUT_FULL == 0 {
                        return;
                    }
                    Hardware.delay();
                }
                panic!("controller did not accept test setup");
            }
            ready();
            Hardware.data(0xf5);
            let mut ack = None;
            for _ in 0..10_000 {
                if Hardware.status() & ps2::OUTPUT_FULL != 0 {
                    ack = Some(Hardware.read());
                    break;
                }
                Hardware.delay();
            }
            assert_eq!(ack, Some(0xfa));
            ready();
            Hardware.command(0x60);
            ready();
            Hardware.data(ps2::KEYBOARD_DISABLED | ps2::AUX_DISABLED | 4);
            ready();
            init();
            assert_eq!(diagnostics().init.error, None);
        });
        inject_all(&[0x1e, 0x9e]);
        assert_eq!(drain(), "a");
    }

    #[test_case]
    fn test_keyboard_poll_recovers_masked_irq_without_duplicate_input() {
        drain();
        let masks = crate::interrupts::with_pics(|pics| unsafe {
            let masks = pics.read_masks();
            pics.write_masks(masks[0] | 2, masks[1]);
            masks
        });
        let before = diagnostics();
        inject_all(&[0x1e, 0x9e]);
        let after = diagnostics();
        assert_eq!(after.irqs, before.irqs);
        assert_eq!(after.polled - before.polled, 2);
        assert_eq!(drain(), "a");
        crate::interrupts::with_pics(|pics| unsafe {
            pics.write_masks(masks[0], masks[1]);
        });
        let start = ticks();
        wait_until("pending IRQ after polling", || ticks() >= start + 3);
        assert_eq!(drain(), "");
    }

    #[test_case]
    fn test_keyboard_filters_protocol_bytes_without_losing_shift_release() {
        drain();
        let before = diagnostics().rejected;
        inject_all(&[
            0xfa, 0xfe, 0xfc, 0xfd, 0, 0xff, 0x2a, 0x1e, 0x9e, 0xaa, 0x1e, 0x9e,
        ]);
        assert_eq!(drain(), "Aa");
        assert_eq!(diagnostics().rejected - before, 6);
    }

    #[test_case]
    fn test_keyboard_shift() {
        drain();
        inject_all(&[0x2a, 0x1e, 0x9e, 0xaa, 0x1e, 0x9e]);
        assert_eq!(drain(), "Aa");
    }

    #[test_case]
    fn test_keyboard_extended_keys_are_skipped() {
        drain();
        // left arrow press and release, then a
        inject_all(&[0xe0, 0x4b, 0xe0, 0xcb, 0x1e, 0x9e]);
        assert_eq!(drain(), "a");
    }

    #[test_case]
    fn test_keyboard_buffer_overflow_drops_new_chars() {
        drain();
        let dropped_before = dropped_chars();
        for _ in 0..BUFFER_SIZE + 6 {
            inject_all(&[0x1e, 0x9e]);
        }
        assert_eq!(drain().len(), BUFFER_SIZE);
        assert_eq!(dropped_chars() - dropped_before, 6);
    }
}
