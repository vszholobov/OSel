//! Time for programs: the timer tick turned into nanoseconds and the Linux `timespec`
//!
//! The kernel counts timer interrupts (`TICK_HZ` of them a second). These pure functions
//! turn a tick count into nanoseconds and encode and decode a `timespec`, so `clock_gettime`
//! and `nanosleep` can be tested on the host without a real timer.

pub const NANOS_PER_SEC: u64 = 1_000_000_000;

/// Nanoseconds in `ticks` of a `hz` timer. `hz` divides a second evenly for the timers
/// OSel uses (100 Hz), so this is exact.
pub fn ticks_to_nanos(ticks: u64, hz: u32) -> u64 {
    ticks.saturating_mul(NANOS_PER_SEC / hz.max(1) as u64)
}

/// The first tick whose time is at least `nanos`, for a sleep deadline. Rounds up: a sleep
/// never ends early, at worst one tick late.
pub fn nanos_to_ticks_ceil(nanos: u64, hz: u32) -> u64 {
    let per_tick = NANOS_PER_SEC / hz.max(1) as u64;
    nanos.div_ceil(per_tick)
}

/// A `timespec` worth of nanoseconds as the 16 bytes Linux x86_64 uses: `tv_sec` then
/// `tv_nsec`, both 8 byte little endian
pub fn encode_timespec(nanos: u64) -> [u8; 16] {
    let mut bytes = [0u8; 16];
    bytes[..8].copy_from_slice(&(nanos / NANOS_PER_SEC).to_le_bytes());
    bytes[8..].copy_from_slice(&(nanos % NANOS_PER_SEC).to_le_bytes());
    bytes
}

/// Nanoseconds from a `timespec`, `None` if it is negative or `tv_nsec` is out of range,
/// as Linux rejects for `nanosleep` (`EINVAL`). Saturates a huge duration.
pub fn decode_timespec(bytes: [u8; 16]) -> Option<u64> {
    let secs = i64::from_le_bytes(bytes[..8].try_into().unwrap());
    let nanos = i64::from_le_bytes(bytes[8..].try_into().unwrap());
    if secs < 0 || !(0..NANOS_PER_SEC as i64).contains(&nanos) {
        return None;
    }
    Some((secs as u64).saturating_mul(NANOS_PER_SEC).saturating_add(nanos as u64))
}

/// A moment the real-time clock reports
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DateTime {
    pub year: u16,
    pub month: u8,
    pub day: u8,
    pub hour: u8,
    pub minute: u8,
    pub second: u8,
}

/// Days since 1970-01-01 for a date, by Howard Hinnant's `days_from_civil`. Works for any
/// proleptic Gregorian date; the RTC only gives sane ones.
fn days_from_civil(year: i64, month: i64, day: i64) -> i64 {
    let year = if month <= 2 { year - 1 } else { year };
    let era = year.div_euclid(400);
    let year_of_era = year - era * 400;
    let day_of_year = (153 * (if month > 2 { month - 3 } else { month + 9 }) + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146097 + day_of_era - 719468
}

/// Nanoseconds since the Unix epoch for a real-time-clock reading
pub fn unix_nanos(time: DateTime) -> u64 {
    let days = days_from_civil(time.year as i64, time.month as i64, time.day as i64);
    let secs = days * 86400 + time.hour as i64 * 3600 + time.minute as i64 * 60 + time.second as i64;
    (secs.max(0) as u64).saturating_mul(NANOS_PER_SEC)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unix_epoch_conversions() {
        let at = |year, month, day, hour, minute, second| unix_nanos(DateTime { year, month, day, hour, minute, second });
        assert_eq!(at(1970, 1, 1, 0, 0, 0), 0);
        assert_eq!(at(1970, 1, 1, 0, 0, 1), NANOS_PER_SEC);
        assert_eq!(at(2000, 1, 1, 0, 0, 0), 946_684_800 * NANOS_PER_SEC, "a known leap-aware epoch");
        assert_eq!(at(2026, 10, 13, 12, 0, 0), 1_791_892_800 * NANOS_PER_SEC);
        assert!(at(2026, 1, 1, 0, 0, 0) > at(2025, 12, 31, 23, 59, 59));
    }

    #[test]
    fn ticks_and_back() {
        assert_eq!(ticks_to_nanos(0, 100), 0);
        assert_eq!(ticks_to_nanos(1, 100), 10_000_000, "one 100 Hz tick is 10 ms");
        assert_eq!(ticks_to_nanos(100, 100), NANOS_PER_SEC);
        assert_eq!(nanos_to_ticks_ceil(0, 100), 0);
        assert_eq!(nanos_to_ticks_ceil(1, 100), 1, "a tiny sleep still waits a whole tick");
        assert_eq!(nanos_to_ticks_ceil(10_000_000, 100), 1);
        assert_eq!(nanos_to_ticks_ceil(10_000_001, 100), 2);
        assert_eq!(ticks_to_nanos(u64::MAX, 100), u64::MAX, "no overflow");
    }

    #[test]
    fn timespec_round_trip() {
        for nanos in [0, 1, 999_999_999, NANOS_PER_SEC, 3 * NANOS_PER_SEC + 42] {
            assert_eq!(decode_timespec(encode_timespec(nanos)), Some(nanos), "{nanos}");
        }
        let bytes = encode_timespec(2 * NANOS_PER_SEC + 500);
        assert_eq!(i64::from_le_bytes(bytes[..8].try_into().unwrap()), 2);
        assert_eq!(i64::from_le_bytes(bytes[8..].try_into().unwrap()), 500);
    }

    #[test]
    fn bad_timespecs_are_rejected() {
        let mut negative_secs = [0u8; 16];
        negative_secs[..8].copy_from_slice(&(-1i64).to_le_bytes());
        assert_eq!(decode_timespec(negative_secs), None);
        let mut big_nanos = [0u8; 16];
        big_nanos[8..].copy_from_slice(&NANOS_PER_SEC.to_le_bytes());
        assert_eq!(decode_timespec(big_nanos), None, "tv_nsec must be below a second");
        let mut negative_nanos = [0u8; 16];
        negative_nanos[8..].copy_from_slice(&(-5i64).to_le_bytes());
        assert_eq!(decode_timespec(negative_nanos), None);
    }
}
