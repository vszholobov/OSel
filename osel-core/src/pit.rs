//! Programmable interval timer (Intel 8253/8254) and legacy PIC arithmetic

/// Input clock of the PIT channels
pub const PIT_FREQUENCY_HZ: u32 = 1_193_182;

/// Reload value for a channel to fire at about `hz` times per second.
/// The PIT can't go slower than ~18.2 Hz or faster than its input clock.
pub fn divisor(hz: u32) -> u16 {
    (PIT_FREQUENCY_HZ / hz.max(1)).clamp(1, u16::MAX as u32) as u16
}

/// Frequency the PIT really runs at with `divisor`
pub fn frequency_millihz(divisor: u16) -> u64 {
    PIT_FREQUENCY_HZ as u64 * 1000 / divisor.max(1) as u64
}

/// Milliseconds that `ticks` interrupts of a `hz` timer take
pub fn ticks_to_ms(ticks: u64, hz: u32) -> u64 {
    ticks * 1000 / hz.max(1) as u64
}

/// Mask bytes for the master and slave PIC enabling only `irqs` (0..16).
/// The cascade line IRQ 2 is opened whenever a slave line is used.
pub fn pic_masks(irqs: &[u8]) -> (u8, u8) {
    let (mut master, mut slave) = (0xffu8, 0xffu8);
    for &irq in irqs {
        match irq {
            0..=7 => master &= !(1 << irq),
            8..=15 => {
                slave &= !(1 << (irq - 8));
                master &= !(1 << 2);
            }
            _ => {}
        }
    }
    (master, slave)
}

/// IRQ 7 and 15 fire spuriously: the in-service register then has the bit clear
pub fn is_spurious(in_service: u8, irq: u8) -> bool {
    in_service & (1 << (irq % 8)) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn divisors() {
        assert_eq!(divisor(100), 11931);
        assert_eq!(divisor(1000), 1193);
        assert_eq!(divisor(PIT_FREQUENCY_HZ), 1);
        assert_eq!(divisor(u32::MAX), 1, "faster than the clock is clamped");
        assert_eq!(divisor(18), 65535, "slower than possible is clamped");
        assert_eq!(divisor(0), 65535);
    }

    #[test]
    fn real_frequency() {
        let millihz = frequency_millihz(divisor(100));
        assert!((99_900..100_100).contains(&millihz), "{millihz}");
        assert_eq!(frequency_millihz(0), PIT_FREQUENCY_HZ as u64 * 1000);
    }

    #[test]
    fn tick_durations() {
        assert_eq!(ticks_to_ms(100, 100), 1000);
        assert_eq!(ticks_to_ms(5, 100), 50);
        assert_eq!(ticks_to_ms(1, 0), 1000, "zero frequency does not divide by zero");
    }

    #[test]
    fn masks() {
        assert_eq!(pic_masks(&[]), (0xff, 0xff));
        assert_eq!(pic_masks(&[0, 1]), (0b1111_1100, 0xff));
        assert_eq!(pic_masks(&[12]), (0b1111_1011, 0b1110_1111), "mouse needs the cascade");
        assert_eq!(pic_masks(&[0, 1, 8, 15, 16]), (0b1111_1000, 0b0111_1110));
    }

    #[test]
    fn spurious_detection() {
        assert!(is_spurious(0b0000_0000, 7));
        assert!(!is_spurious(0b1000_0000, 7));
        assert!(is_spurious(0b0000_0001, 15));
        assert!(!is_spurious(0b1000_0000, 15));
    }
}
