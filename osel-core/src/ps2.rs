//! Bounded i8042 / PS/2 keyboard startup, independent of port I/O and IRQ delivery.

pub const OUTPUT_FULL: u8 = 1;
pub const INPUT_FULL: u8 = 2;
pub const AUX: u8 = 0x20;
pub const BAD_DATA: u8 = 0xc0;
pub const IRQ1: u8 = 1;
pub const IRQ12: u8 = 2;
pub const KEYBOARD_DISABLED: u8 = 0x10;
pub const AUX_DISABLED: u8 = 0x20;
pub const TRANSLATE: u8 = 0x40;

/// `delay` must work before timer interrupts are enabled. All waits have a budget.
pub trait Io {
    fn status(&mut self) -> u8;
    fn read(&mut self) -> u8;
    fn command(&mut self, value: u8);
    fn data(&mut self, value: u8);
    fn delay(&mut self);
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    Absent,
    WriteTimeout,
    ReadTimeout,
    BadData,
    DrainTimeout,
    Reply(u8),
    ResendLimit,
    ConfigMismatch(u8),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Stage {
    Detect,
    DisablePorts,
    ReadConfig,
    WriteConfig,
    EnablePort,
    DisableScanning,
    SelectSet2,
    EnableScanning,
    EnableIrq,
    Ready,
}

#[derive(Clone, Copy, Debug)]
pub struct Report {
    pub stage: Stage,
    pub error: Option<Error>,
    pub config_before: Option<u8>,
    pub config_after: Option<u8>,
    pub last_reply: Option<u8>,
    pub discarded: usize,
    pub retries: usize,
}

impl Report {
    pub const fn new() -> Self {
        Self {
            stage: Stage::Detect,
            error: None,
            config_before: None,
            config_after: None,
            last_reply: None,
            discarded: 0,
            retries: 0,
        }
    }
}

impl Default for Report {
    fn default() -> Self {
        Self::new()
    }
}

struct Controller<'a, T> {
    io: &'a mut T,
    budget: usize,
    report: Report,
}

/// Initialize with CPU interrupts disabled and no other reader of ports 0x60/0x64.
/// No controller self-test/reset: laptop ECs may reset unrelated state on that path.
pub fn initialize(io: &mut impl Io, budget: usize) -> Report {
    let mut c = Controller {
        io,
        budget,
        report: Report::new(),
    };
    if let Err(error) = c.start() {
        c.report.error = Some(error);
    }
    c.report
}

impl<T: Io> Controller<'_, T> {
    fn status(&mut self) -> Result<u8, Error> {
        match self.io.status() {
            0xff => Err(Error::Absent),
            status => Ok(status),
        }
    }

    fn writable(&mut self) -> Result<(), Error> {
        for _ in 0..self.budget {
            if self.status()? & INPUT_FULL == 0 {
                return Ok(());
            }
            self.io.delay();
        }
        Err(Error::WriteTimeout)
    }

    fn command(&mut self, value: u8) -> Result<(), Error> {
        self.writable()?;
        self.io.command(value);
        Ok(())
    }

    fn data(&mut self, value: u8) -> Result<(), Error> {
        self.writable()?;
        self.io.data(value);
        Ok(())
    }

    fn reply(&mut self) -> Result<u8, Error> {
        for _ in 0..self.budget {
            let status = self.status()?;
            if status & OUTPUT_FULL != 0 {
                let byte = self.io.read();
                if status & AUX != 0 {
                    self.report.discarded += 1;
                } else {
                    self.report.last_reply = Some(byte);
                    if status & BAD_DATA != 0 {
                        return Err(Error::BadData);
                    }
                    return Ok(byte);
                }
            }
            self.io.delay();
        }
        Err(Error::ReadTimeout)
    }

    fn drain(&mut self) -> Result<(), Error> {
        for _ in 0..64 {
            if self.status()? & OUTPUT_FULL == 0 {
                return Ok(());
            }
            self.io.read();
            self.report.discarded += 1;
            self.io.delay();
        }
        if self.status()? & OUTPUT_FULL == 0 {
            Ok(())
        } else {
            Err(Error::DrainTimeout)
        }
    }

    fn read_config(&mut self) -> Result<u8, Error> {
        self.command(0x20)?;
        self.reply()
    }

    fn write_config(&mut self, value: u8) -> Result<(), Error> {
        self.command(0x60)?;
        self.data(value)?;
        // Wait for the EC to consume the parameter before allowing another operation.
        self.writable()
    }

    fn keyboard(&mut self, value: u8, allow_scancodes: bool) -> Result<(), Error> {
        for _ in 0..3 {
            self.data(value)?;
            // Before F5 is acknowledged an already pressed key may still be in flight.
            // Do not let such a byte masquerade as a command ACK or wait indefinitely.
            for _ in 0..16 {
                match self.reply()? {
                    0xfa => return Ok(()),
                    0xfe => {
                        self.report.retries += 1;
                        break;
                    }
                    byte if allow_scancodes && !matches!(byte, 0xfc | 0xfd | 0x00 | 0xff) => {
                        self.report.discarded += 1;
                    }
                    byte => return Err(Error::Reply(byte)),
                }
            }
            if self.report.last_reply != Some(0xfe) {
                return Err(Error::Reply(self.report.last_reply.unwrap_or(0)));
            }
        }
        Err(Error::ResendLimit)
    }

    fn start(&mut self) -> Result<(), Error> {
        self.status()?;
        self.report.stage = Stage::DisablePorts;
        self.command(0xad)?;
        self.command(0xa7)?;
        self.writable()?;
        self.drain()?;
        self.report.stage = Stage::ReadConfig;
        let before = self.read_config()?;
        self.report.config_before = Some(before);
        // Preserve firmware-owned bits, disable AUX and IRQs, enable set 2 -> set 1.
        let quiet = (before | AUX_DISABLED | TRANSLATE) & !(IRQ1 | IRQ12);
        self.report.stage = Stage::WriteConfig;
        self.write_config(quiet)?;
        self.report.stage = Stage::EnablePort;
        self.command(0xae)?;
        self.report.stage = Stage::DisableScanning;
        self.keyboard(0xf5, true)?;
        self.drain()?;
        self.report.stage = Stage::SelectSet2;
        self.keyboard(0xf0, false)?;
        self.keyboard(0x02, false)?;
        // Verify the command byte before enabling scanning, so keypresses cannot be
        // confused with its reply. F4 is the final transaction with the keyboard.
        self.report.stage = Stage::EnableIrq;
        let active = (quiet | IRQ1) & !KEYBOARD_DISABLED;
        self.write_config(active)?;
        let after = self.read_config()?;
        self.report.config_after = Some(after);
        let relevant = IRQ1 | IRQ12 | KEYBOARD_DISABLED | AUX_DISABLED | TRANSLATE;
        if after & relevant != active & relevant {
            return Err(Error::ConfigMismatch(after));
        }
        self.report.stage = Stage::EnableScanning;
        self.keyboard(0xf4, false)?;
        self.report.stage = Stage::Ready;
        Ok(())
    }
}

/// A runtime byte may be decoded only if it actually came from the keyboard.
pub fn is_scancode(status: u8, byte: u8) -> bool {
    status != 0xff
        && status & OUTPUT_FULL != 0
        && status & (AUX | BAD_DATA) == 0
        && !matches!(byte, 0xfa | 0xfe | 0xfc | 0xfd | 0x00 | 0xff)
    // 0xaa is a valid set-1 left-Shift release; do not filter it as a BAT reply.
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct Fake {
        config: u8,
        output: VecDeque<(u8, u8)>,
        commands: Vec<u8>,
        data: Vec<u8>,
        writing_config: bool,
        status_override: Option<u8>,
        keyboard_replies: VecDeque<Vec<(u8, u8)>>,
        drop_replies: bool,
        delays: usize,
        ignore_config: bool,
    }

    impl Fake {
        fn new() -> Self {
            Self {
                config: 0x07,
                output: VecDeque::new(),
                commands: vec![],
                data: vec![],
                writing_config: false,
                status_override: None,
                keyboard_replies: VecDeque::new(),
                drop_replies: false,
                delays: 0,
                ignore_config: false,
            }
        }
    }

    impl Io for Fake {
        fn status(&mut self) -> u8 {
            self.status_override
                .unwrap_or_else(|| self.output.front().map_or(0, |b| b.0 | OUTPUT_FULL))
        }
        fn read(&mut self) -> u8 {
            self.output.pop_front().map_or(0, |b| b.1)
        }
        fn command(&mut self, value: u8) {
            self.commands.push(value);
            match value {
                0xad => self.config |= KEYBOARD_DISABLED,
                0xa7 => self.config |= AUX_DISABLED,
                0xae => self.config &= !KEYBOARD_DISABLED,
                0x20 if !self.drop_replies => self.output.push_back((0, self.config)),
                0x60 => self.writing_config = true,
                _ => {}
            }
        }
        fn data(&mut self, value: u8) {
            self.data.push(value);
            if self.writing_config {
                if !self.ignore_config {
                    self.config = value;
                }
                self.writing_config = false;
            } else {
                let replies = self
                    .keyboard_replies
                    .pop_front()
                    .unwrap_or_else(|| vec![(0, 0xfa)]);
                self.output.extend(replies);
            }
        }
        fn delay(&mut self) {
            self.delays += 1;
        }
    }

    #[test]
    fn initializes_disabled_untranslated_keyboard_and_preserves_system_bit() {
        let mut io = Fake::new();
        io.output.push_back((0, 0x9c));
        let r = initialize(&mut io, 8);
        assert_eq!(r.stage, Stage::Ready);
        assert_eq!(r.error, None);
        assert_eq!(r.config_after, Some(0x65));
        assert_eq!(r.discarded, 1);
        assert_eq!(io.data, [0x74, 0xf5, 0xf0, 2, 0x65, 0xf4]);
        assert!(!io.commands.contains(&0xaa));
        assert!(!io.commands.contains(&0xd1)); // never touch the reset/A20 output port
    }

    #[test]
    fn retries_only_resend_and_skips_inflight_keys_and_aux() {
        let mut io = Fake::new();
        io.keyboard_replies
            .push_back(vec![(0, 0x1e), (AUX, 0xfa), (0, 0xfe)]);
        let r = initialize(&mut io, 8);
        assert_eq!(r.error, None);
        assert_eq!(r.retries, 1);
        assert_eq!(r.discarded, 2);
        assert_eq!(io.data.iter().filter(|&&b| b == 0xf5).count(), 2);
    }

    #[test]
    fn absent_and_busy_controller_are_bounded() {
        for (status, error) in [(0xff, Error::Absent), (INPUT_FULL, Error::WriteTimeout)] {
            let mut io = Fake::new();
            io.status_override = Some(status);
            assert_eq!(initialize(&mut io, 8).error, Some(error));
            assert!(io.commands.is_empty());
            assert!(io.delays <= 8);
        }
    }

    #[test]
    fn stuck_output_is_bounded() {
        let mut io = Fake::new();
        io.status_override = Some(OUTPUT_FULL);
        assert_eq!(initialize(&mut io, 8).error, Some(Error::DrainTimeout));
        assert_eq!(io.delays, 64);
    }

    #[test]
    fn missing_config_reply_is_bounded() {
        let mut io = Fake::new();
        io.drop_replies = true;
        let r = initialize(&mut io, 8);
        assert_eq!(r.stage, Stage::ReadConfig);
        assert_eq!(r.error, Some(Error::ReadTimeout));
        assert_eq!(io.delays, 8);
    }

    #[test]
    fn keyboard_errors_and_missing_acks_are_reported() {
        for (reply, error) in [
            (vec![], Error::ReadTimeout),
            (vec![(BAD_DATA, 0xfa)], Error::BadData),
            (vec![(0, 0xfc)], Error::Reply(0xfc)),
        ] {
            let mut io = Fake::new();
            io.keyboard_replies.push_back(reply);
            let r = initialize(&mut io, 8);
            assert_eq!(r.stage, Stage::DisableScanning);
            assert_eq!(r.error, Some(error));
        }
    }

    #[test]
    fn resend_storm_does_not_hang() {
        let mut io = Fake::new();
        for _ in 0..3 {
            io.keyboard_replies.push_back(vec![(0, 0xfe)]);
        }
        assert_eq!(initialize(&mut io, 8).error, Some(Error::ResendLimit));
    }

    #[test]
    fn verifies_translation_and_irq_configuration() {
        let mut io = Fake::new();
        io.ignore_config = true;
        let r = initialize(&mut io, 8);
        assert_eq!(r.stage, Stage::EnableIrq);
        assert!(matches!(r.error, Some(Error::ConfigMismatch(_))));
        assert!(!io.data.contains(&0xf4));
    }

    #[test]
    fn runtime_filters_aux_errors_and_protocol_but_keeps_shift_release() {
        assert!(is_scancode(1, 0x1e));
        assert!(is_scancode(1, 0xaa));
        for status in [0, 0xff, OUTPUT_FULL | AUX, OUTPUT_FULL | BAD_DATA] {
            assert!(!is_scancode(status, 0x1e));
        }
        for byte in [0xfa, 0xfe, 0xfc, 0xfd, 0, 0xff] {
            assert!(!is_scancode(1, byte));
        }
    }
}
