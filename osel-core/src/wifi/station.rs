//! Minimal legacy 2.4 GHz STA management, without HT/VHT, PMF or roaming.
use super::ieee80211::{AccessPoint, RSN_IE};
use crate::errno::{Errno, KResult};
use alloc::{vec, vec::Vec};

pub fn supported(ap: &AccessPoint) -> KResult<()> {
    if ap.ssid_len > 32
        || ap.rates_len > 12
        || ap.beacon_interval == 0
        || ap.dtim == 0
        || (ap.frequency.saturating_sub(2412)) % 5 != 0
        || !ap.wpa2()
        || ap.hidden()
        || ap.capabilities & 3 != 1
        || ap.rates_len == 0
        || ap.unsupported_basic_rate
        || !(2412..=2462).contains(&ap.frequency)
    {
        return Err(Errno::EOPNOTSUPP);
    }
    Ok(())
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
