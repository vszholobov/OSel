//! Human readable reports of CPU exceptions for the panic screen

use core::fmt;
use x86_64::structures::idt::PageFaultErrorCode;

/// CPU state saved by an exception
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Frame {
    pub rip: u64,
    pub rsp: u64,
    pub cs: u16,
    pub ss: u16,
    pub rflags: u64,
}

/// What else is known about an exception
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Details {
    None,
    /// The error code is a plain number
    ErrorCode(u64),
    /// The error code refers to a segment selector or descriptor table entry
    Selector(u64),
    /// `address` is the raw content of CR2
    PageFault { code: u64, address: u64 },
}

pub struct ExceptionReport<'a> {
    pub name: &'a str,
    pub frame: Frame,
    pub details: Details,
}

impl fmt::Display for ExceptionReport<'_> {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let frame = &self.frame;
        write!(
            f,
            "EXCEPTION: {}\n  rip: {:#018x}  rsp: {:#018x}\n  cs: {:#06x}  ss: {:#06x}  rflags: {:#x}",
            self.name, frame.rip, frame.rsp, frame.cs, frame.ss, frame.rflags
        )?;
        match self.details {
            Details::None => Ok(()),
            Details::ErrorCode(code) => write!(f, "\n  error code: {:#x}", code),
            Details::Selector(code) => write!(f, "\n  error code: {:#x} ({})", code, SelectorError(code)),
            Details::PageFault { code, address } => write!(
                f,
                "\n  error code: {:#x}\n  accessed address: {:#x}{}\n  reason: {}",
                code,
                address,
                if is_canonical(address) { "" } else { " (non-canonical)" },
                PageFaultCause(code)
            ),
        }
    }
}

/// Error code of #NP, #SS, #GP, #TS: which descriptor caused the fault
pub struct SelectorError(pub u64);

impl fmt::Display for SelectorError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let code = self.0;
        if code == 0 {
            return f.write_str("not caused by a segment");
        }
        let index = (code >> 3) & 0x1fff;
        let table = if code & 0b10 != 0 {
            "IDT"
        } else if code & 0b100 != 0 {
            "LDT"
        } else {
            "GDT"
        };
        write!(f, "{} entry {} ({:#x})", table, index, index)?;
        if code & 1 != 0 {
            f.write_str(", external event")?;
        }
        Ok(())
    }
}

/// Page fault error code as a sentence: what, on which access, by whom
pub struct PageFaultCause(pub u64);

impl fmt::Display for PageFaultCause {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let code = PageFaultErrorCode::from_bits_retain(self.0);
        let cause = if code.contains(PageFaultErrorCode::PROTECTION_VIOLATION) {
            "protection violation"
        } else {
            "page not present"
        };
        let access = if code.contains(PageFaultErrorCode::INSTRUCTION_FETCH) {
            "instruction fetch"
        } else if code.contains(PageFaultErrorCode::CAUSED_BY_WRITE) {
            "write"
        } else {
            "read"
        };
        let mode = if code.contains(PageFaultErrorCode::USER_MODE) { "user" } else { "kernel" };
        write!(f, "{} on {} by {}", cause, access, mode)?;
        for (flag, name) in [
            (PageFaultErrorCode::MALFORMED_TABLE, "reserved bit set in a page table"),
            (PageFaultErrorCode::PROTECTION_KEY, "protection key"),
            (PageFaultErrorCode::SHADOW_STACK, "shadow stack"),
            (PageFaultErrorCode::SGX, "SGX"),
        ] {
            if code.contains(flag) {
                write!(f, ", {}", name)?;
            }
        }
        Ok(())
    }
}

fn is_canonical(address: u64) -> bool {
    let top = address >> 47;
    top == 0 || top == 0x1ffff
}

/// Formats into a fixed buffer without allocating, cutting off what doesn't fit.
/// Usable where the heap may be broken, like in panic handlers.
pub struct FixedBuf<const N: usize> {
    bytes: [u8; N],
    len: usize,
    truncated: bool,
}

impl<const N: usize> FixedBuf<N> {
    pub const fn new() -> Self {
        FixedBuf { bytes: [0; N], len: 0, truncated: false }
    }

    pub fn as_str(&self) -> &str {
        // only whole characters are ever written
        core::str::from_utf8(&self.bytes[..self.len]).unwrap_or("")
    }

    pub fn truncated(&self) -> bool {
        self.truncated
    }
}

impl<const N: usize> Default for FixedBuf<N> {
    fn default() -> Self {
        Self::new()
    }
}

impl<const N: usize> fmt::Write for FixedBuf<N> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        if self.truncated {
            // appending after a gap would make the text misleading
            return Ok(());
        }
        for c in s.chars() {
            let mut encoded = [0; 4];
            let encoded = c.encode_utf8(&mut encoded).as_bytes();
            if self.len + encoded.len() > N {
                self.truncated = true;
                // keep formatting going, the rest is silently dropped
                return Ok(());
            }
            self.bytes[self.len..self.len + encoded.len()].copy_from_slice(encoded);
            self.len += encoded.len();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::format;
    use core::fmt::Write;

    const FRAME: Frame = Frame { rip: 0xffff_8000_0001_8c11, rsp: 0xffff_8080_0001_4f80, cs: 8, ss: 0x10, rflags: 0x10286 };

    fn report(name: &str, details: Details) -> alloc::string::String {
        format!("{}", ExceptionReport { name, frame: FRAME, details })
    }

    #[test]
    fn report_without_error_code() {
        assert_eq!(
            report("DIVIDE ERROR", Details::None),
            "EXCEPTION: DIVIDE ERROR\n  rip: 0xffff800000018c11  rsp: 0xffff808000014f80\n  cs: 0x0008  ss: 0x0010  rflags: 0x10286"
        );
    }

    #[test]
    fn report_with_error_code() {
        assert!(report("DOUBLE FAULT", Details::ErrorCode(0)).ends_with("\n  error code: 0x0"));
    }

    #[test]
    fn report_with_selector() {
        let text = report("SEGMENT NOT PRESENT", Details::Selector(0x282));
        assert!(text.ends_with("error code: 0x282 (IDT entry 80 (0x50))"), "{text}");
    }

    #[test]
    fn report_page_fault() {
        let text = report("PAGE FAULT", Details::PageFault { code: 0, address: 0xdead_beef_000 });
        assert!(text.contains("\n  error code: 0x0\n  accessed address: 0xdeadbeef000\n  reason: page not present on read by kernel"));
        let text = report("PAGE FAULT", Details::PageFault { code: 0, address: 0x8000_0000_0000 });
        assert!(text.contains("0x800000000000 (non-canonical)"), "{text}");
    }

    #[test]
    fn selector_errors() {
        assert_eq!(format!("{}", SelectorError(0)), "not caused by a segment");
        assert_eq!(format!("{}", SelectorError(0x10)), "GDT entry 2 (0x2)");
        assert_eq!(format!("{}", SelectorError(0x1230)), "GDT entry 582 (0x246)");
        assert_eq!(format!("{}", SelectorError(0x0c)), "LDT entry 1 (0x1)");
        assert_eq!(format!("{}", SelectorError(0x13a)), "IDT entry 39 (0x27)");
        assert_eq!(format!("{}", SelectorError(0x13b)), "IDT entry 39 (0x27), external event");
    }

    #[test]
    fn page_fault_causes() {
        let cause = |bits: u64| format!("{}", PageFaultCause(bits));
        assert_eq!(cause(0b0000), "page not present on read by kernel");
        assert_eq!(cause(0b0001), "protection violation on read by kernel");
        assert_eq!(cause(0b0010), "page not present on write by kernel");
        assert_eq!(cause(0b0111), "protection violation on write by user");
        assert_eq!(cause(0b1_0000), "page not present on instruction fetch by kernel");
        assert_eq!(cause(0b1_0011), "protection violation on instruction fetch by kernel", "fetch wins over write");
        assert_eq!(cause(0b1001), "protection violation on read by kernel, reserved bit set in a page table");
        assert_eq!(cause(1 << 5), "page not present on read by kernel, protection key");
        assert_eq!(cause(1 << 15), "page not present on read by kernel, SGX");
    }

    #[test]
    fn canonical_addresses() {
        assert!(is_canonical(0));
        assert!(is_canonical(0x7fff_ffff_ffff));
        assert!(!is_canonical(0x8000_0000_0000));
        assert!(!is_canonical(0xffff_7fff_ffff_ffff));
        assert!(is_canonical(0xffff_8000_0000_0000));
        assert!(is_canonical(u64::MAX));
    }

    #[test]
    fn fixed_buf_formats() {
        let mut buf = FixedBuf::<32>::new();
        write!(buf, "{} + {} = {}", 2, 2, 4).unwrap();
        assert_eq!(buf.as_str(), "2 + 2 = 4");
        assert!(!buf.truncated());
    }

    #[test]
    fn fixed_buf_truncates_on_char_boundaries() {
        let mut buf = FixedBuf::<5>::new();
        write!(buf, "abЖЖЖ").unwrap();
        assert_eq!(buf.as_str(), "abЖ", "a 2 byte char that doesn't fit is dropped whole");
        assert!(buf.truncated());
        write!(buf, "c").unwrap();
        assert_eq!(buf.as_str(), "abЖ", "nothing is appended after truncation");
    }

    #[test]
    fn fixed_buf_empty() {
        let buf = FixedBuf::<0>::new();
        assert_eq!(buf.as_str(), "");
    }
}
