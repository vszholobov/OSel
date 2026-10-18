//! Minimal legacy 2.4 GHz STA management, without HT/VHT, PMF or roaming.
use super::ieee80211::{AccessPoint, RSN_IE};
use crate::errno::{Errno, KResult};
use alloc::{vec, vec::Vec};

const IDLE_CHECK_NS: u64 = 30_000_000_000;
const PROBE_INTERVAL_NS: u64 = 1_000_000_000;
const PROBE_LIMIT: u8 = 3;

/// Firmware beacon loss starts a bounded AP check, not an immediate disconnect.
/// Idle checks also cover firmware which reports beacon loss only once.
#[derive(Default, Debug)]
pub struct LinkMonitor {
    next_check: Option<u64>,
    deadline: Option<u64>,
    probes: u8,
}
impl LinkMonitor {
    pub fn start(&mut self, now: u64) {
        self.next_check = Some(now.saturating_add(IDLE_CHECK_NS));
        self.deadline = None;
        self.probes = 0;
    }
    pub fn activity(&mut self, now: u64) {
        if self.next_check.is_some() {
            self.start(now);
        }
    }
    pub fn beacon_missed(&mut self, now: u64) {
        if self.next_check.is_some() && self.deadline.is_none() {
            self.deadline = Some(now.saturating_add(PROBE_INTERVAL_NS * u64::from(PROBE_LIMIT)));
            self.next_check = Some(now);
            self.probes = 0;
        }
    }
    pub fn probe_due(&mut self, now: u64) -> KResult<bool> {
        if self.deadline.is_some_and(|deadline| now >= deadline) {
            return Err(Errno::ENETUNREACH);
        }
        if !self.next_check.is_some_and(|next| now >= next) {
            return Ok(false);
        }
        self.beacon_missed(now);
        Ok(self.probes < PROBE_LIMIT)
    }
    pub fn probe_sent(&mut self, now: u64) {
        self.probes = self.probes.saturating_add(1);
        self.next_check = Some(now.saturating_add(PROBE_INTERVAL_NS));
    }
    pub fn probes(&self) -> u8 {
        self.probes
    }
}

pub fn supported(ap: &AccessPoint) -> KResult<()> {
    unsupported_reason(ap).map_or(Ok(()), |_| Err(Errno::EOPNOTSUPP))
}
/// The same admission policy drives diagnostics and association; never imply a
/// discovered AP is connectable merely because it advertises an RSN element.
pub fn unsupported_reason(ap: &AccessPoint) -> Option<&'static str> {
    if ap.ssid_len > 32 || ap.rates_len > 12 || ap.beacon_interval == 0 || ap.dtim == 0 {
        return Some("invalid AP metadata");
    }
    if !(2412..=2462).contains(&ap.frequency) || (ap.frequency - 2412) % 5 != 0 {
        return Some("connection requires channels 1-11");
    }
    if ap.hidden() {
        return Some("hidden SSID");
    }
    if ap.capabilities & 3 != 1 {
        return Some("infrastructure AP required");
    }
    if ap.capabilities & 0x10 == 0 {
        return Some("open network; WPA2 required");
    }
    let Some(rsn) = ap.rsn else {
        return Some("no WPA2 RSN; WPA1/WEP unsupported");
    };
    if !rsn.group_ccmp {
        return Some("group cipher requires CCMP/AES");
    }
    if !rsn.pairwise_ccmp {
        return Some("pairwise cipher requires CCMP/AES");
    }
    if !rsn.psk {
        return Some("WPA2-Personal PSK required");
    }
    if rsn.pmf_required {
        return Some("mandatory PMF unsupported");
    }
    if ap.rates_len == 0 || ap.unsupported_basic_rate {
        return Some("unsupported basic rates");
    }
    None
}
fn header(subtype: u8, station: [u8; 6], ap: [u8; 6], sequence: u16) -> Vec<u8> {
    let mut frame = vec![0; 24];
    frame[0] = subtype;
    frame[4..10].copy_from_slice(&ap);
    frame[10..16].copy_from_slice(&station);
    frame[16..22].copy_from_slice(&ap);
    frame[22..24].copy_from_slice(&((sequence & 0xfff) << 4).to_le_bytes());
    frame
}
pub fn authenticate(station: [u8; 6], ap: [u8; 6], sequence: u16) -> Vec<u8> {
    let mut frame = header(0xb0, station, ap, sequence);
    frame.extend_from_slice(&[0, 0, 1, 0, 0, 0]);
    frame
}
/// A directed management probe on the already-admitted home channel. No RSN,
/// credentials or key material is sent; the normal HTT completion owns its DMA.
pub fn probe_request(station: [u8; 6], ap: &AccessPoint, sequence: u16) -> KResult<Vec<u8>> {
    supported(ap)?;
    let mut frame = header(0x40, station, ap.bssid, sequence);
    frame.extend_from_slice(&[0, ap.ssid_len]);
    frame.extend_from_slice(ap.ssid());
    let rates = &ap.rates[..usize::from(ap.rates_len)];
    let first = rates.len().min(8);
    frame.extend_from_slice(&[1, first as u8]);
    frame.extend(rates[..first].iter().map(|rate| rate & 0x7f));
    if rates.len() > first {
        frame.extend_from_slice(&[50, (rates.len() - first) as u8]);
        frame.extend(rates[first..].iter().map(|rate| rate & 0x7f));
    }
    Ok(frame)
}
pub fn associate(station: [u8; 6], ap: &AccessPoint, sequence: u16) -> KResult<Vec<u8>> {
    supported(ap)?;
    let mut frame = header(0, station, ap.bssid, sequence);
    let capabilities = 0x11 | (ap.capabilities & 0x420); // ESS, privacy; AP's short preamble/slot
    frame.extend_from_slice(&capabilities.to_le_bytes());
    frame.extend_from_slice(&1u16.to_le_bytes()); // listen interval
    frame.extend_from_slice(&[0, ap.ssid_len]);
    frame.extend_from_slice(ap.ssid());
    let rates = &ap.rates[..ap.rates_len as usize];
    let first = rates.len().min(8);
    frame.extend_from_slice(&[1, first as u8]);
    frame.extend_from_slice(&rates[..first]);
    if rates.len() > first {
        frame.extend_from_slice(&[50, (rates.len() - first) as u8]);
        frame.extend_from_slice(&rates[first..]);
    }
    frame.extend_from_slice(&RSN_IE);
    Ok(frame)
}
pub enum Response {
    Authenticated,
    Associated(u16),
    Disconnected(u16),
    Other,
}
pub fn response(frame: &[u8], station: [u8; 6], ap: [u8; 6]) -> KResult<Response> {
    if frame.len() < 24
        || frame[1] & 0x47 != 0
        || frame[4..10] != station
        || frame[10..16] != ap
        || frame[16..22] != ap
    {
        return Ok(Response::Other);
    }
    let word = |offset| -> KResult<u16> {
        Ok(u16::from_le_bytes(
            frame
                .get(offset..offset + 2)
                .ok_or(Errno::EIO)?
                .try_into()
                .unwrap(),
        ))
    };
    match frame[0] {
        0xb0 => {
            if word(24)? != 0 || word(26)? != 2 || word(28)? != 0 {
                return Err(Errno::EACCES);
            }
            Ok(Response::Authenticated)
        }
        0x10 => {
            if word(26)? != 0 {
                return Err(Errno::EACCES);
            }
            let aid = word(28)? & 0x3fff;
            if !(1..=2007).contains(&aid) {
                return Err(Errno::EIO);
            }
            Ok(Response::Associated(aid))
        }
        0xa0 | 0xc0 => Ok(Response::Disconnected(word(24)?)),
        _ => Ok(Response::Other),
    }
}
/// CPU RNG adapter supplies each word; there is no time/seed fallback.
pub fn nonce(mut source: impl FnMut() -> Option<u64>, output: &mut [u8; 32]) -> KResult<()> {
    output.fill(0);
    let mut words = [0u64; 4];
    for i in 0..4 {
        let mut next = None;
        for _ in 0..10 {
            if let Some(word) = source() {
                next = Some(word);
                break;
            }
        }
        let Some(word) = next else {
            return Err(Errno::EIO);
        };
        if word == 0 || word == u64::MAX || words[..i].contains(&word) {
            return Err(Errno::EIO);
        }
        words[i] = word;
    }
    for (word, bytes) in words.iter().zip(output.chunks_exact_mut(8)) {
        bytes.copy_from_slice(&word.to_le_bytes());
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn management_responses_are_bound_to_bssid_station_status_and_aid() {
        let sta = [2, 3, 4, 5, 6, 7];
        let ap = [2, 3, 4, 5, 6, 8];
        let mut auth = authenticate(ap, sta, 2);
        auth[16..22].copy_from_slice(&ap);
        auth[26] = 2;
        assert!(matches!(
            response(&auth, sta, ap).unwrap(),
            Response::Authenticated
        ));
        auth[4] ^= 2;
        assert!(matches!(response(&auth, sta, ap).unwrap(), Response::Other));
        auth[4] ^= 2;
        auth[28] = 1;
        assert!(response(&auth, sta, ap).is_err());
        auth[0] = 0x10;
        auth[26] = 0;
        auth[28..30].copy_from_slice(&0xc123u16.to_le_bytes());
        assert!(matches!(
            response(&auth, sta, ap).unwrap(),
            Response::Associated(0x123)
        ));
        for end in 24..30 {
            assert!(response(&auth[..end], sta, ap).is_err());
        }
    }
    #[test]
    fn missing_stuck_and_repeating_hardware_rng_never_produce_a_nonce() {
        for value in [None, Some(0), Some(u64::MAX), Some(7)] {
            let mut output = [0xaa; 32];
            assert!(nonce(|| value, &mut output).is_err());
            assert_eq!(output, [0; 32]);
        }
        let mut i = 0;
        let mut output = [0; 32];
        nonce(
            || {
                i += 1;
                if i % 2 == 1 { None } else { Some(i) }
            },
            &mut output,
        )
        .unwrap();
        assert_eq!(output[0], 2);
        assert_eq!(output[24], 8);
    }
}
