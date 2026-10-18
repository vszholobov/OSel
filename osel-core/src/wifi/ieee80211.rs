//! Bounded management-frame and RSN parsing for a WPA2-PSK/CCMP station.
use crate::errno::{Errno, KResult};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rsn {
    pub group_ccmp: bool,
    pub pairwise_ccmp: bool,
    pub psk: bool,
    pub pmf_required: bool,
}
impl Rsn {
    pub fn supported(self) -> bool {
        self.group_ccmp && self.pairwise_ccmp && self.psk && !self.pmf_required
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessPoint {
    pub bssid: [u8; 6],
    pub ssid: [u8; 32],
    pub ssid_len: u8,
    pub frequency: u16,
    pub rssi: i32,
    pub capabilities: u16,
    pub beacon_interval: u16,
    pub dtim: u8,
    pub rates: [u8; 12],
    pub rates_len: u8,
    pub unsupported_basic_rate: bool,
    pub rsn: Option<Rsn>,
}
impl AccessPoint {
    pub fn ssid(&self) -> &[u8] {
        &self.ssid[..self.ssid_len as usize]
    }
    pub fn hidden(&self) -> bool {
        self.ssid().iter().all(|&byte| byte == 0)
    }
    pub fn wpa2(&self) -> bool {
        self.capabilities & 0x10 != 0 && self.rsn.is_some_and(Rsn::supported)
    }
}
struct Cursor<'a>(&'a [u8]);
impl<'a> Cursor<'a> {
    fn take(&mut self, length: usize) -> KResult<&'a [u8]> {
        let bytes = self.0.get(..length).ok_or(Errno::EIO)?;
        self.0 = &self.0[length..];
        Ok(bytes)
    }
    fn u16(&mut self) -> KResult<u16> {
        Ok(u16::from_le_bytes(self.take(2)?.try_into().unwrap()))
    }
}
pub fn rsn(bytes: &[u8]) -> KResult<Rsn> {
    let mut input = Cursor(bytes);
    if input.u16()? != 1 {
        return Err(Errno::EOPNOTSUPP);
    }
    let group_ccmp = input.take(4)? == [0, 0x0f, 0xac, 4];
    let mut pairwise_ccmp = false;
    let count = input.u16()?;
    if count == 0 || count > 16 {
        return Err(Errno::EIO);
    }
    for _ in 0..count {
        pairwise_ccmp |= input.take(4)? == [0, 0x0f, 0xac, 4];
    }
    let mut psk = false;
    let count = input.u16()?;
    if count == 0 || count > 16 {
        return Err(Errno::EIO);
    }
    for _ in 0..count {
        psk |= input.take(4)? == [0, 0x0f, 0xac, 2];
    }
    let capabilities = if input.0.is_empty() { 0 } else { input.u16()? };
    if !input.0.is_empty() {
        let count = input.u16()? as usize;
        if count > 16 {
            return Err(Errno::EIO);
        }
        input.take(count * 16)?;
        if !input.0.is_empty() {
            input.take(4)?;
        } // optional group management suite
    }
    if !input.0.is_empty() {
        return Err(Errno::EIO);
    }
    Ok(Rsn {
        group_ccmp,
        pairwise_ccmp,
        psk,
        pmf_required: capabilities & (1 << 6) != 0,
    })
}
/// RX metadata identifies the tuned channel, which can hear neighboring BSSes.
/// Use the AP's advertised primary channel, without crossing the received band.
fn primary_frequency(channel: u8, received: u16) -> KResult<u16> {
    match (received, channel) {
        (2412..=2484, 1..=13) => Ok(2407 + u16::from(channel) * 5),
        (2412..=2484, 14) => Ok(2484),
        (5180..=5980, 36..=196) => Ok(5000 + u16::from(channel) * 5),
        _ => Err(Errno::EIO),
    }
}
pub fn beacon(frame: &[u8], frequency: u16, rssi: i32) -> KResult<AccessPoint> {
    if frame.len() < 36 || !matches!(frame[0], 0x80 | 0x50) || frame[1] & 0x43 != 0 {
        return Err(Errno::EIO);
    }
    let bssid: [u8; 6] = frame[16..22].try_into().unwrap();
    if bssid == [0; 6] || bssid[0] & 1 != 0 {
        return Err(Errno::EIO);
    }
    let mut result = AccessPoint {
        bssid,
        ssid: [0; 32],
        ssid_len: 0,
        frequency,
        rssi,
        capabilities: u16::from_le_bytes(frame[34..36].try_into().unwrap()),
        beacon_interval: u16::from_le_bytes(frame[32..34].try_into().unwrap()),
        dtim: 1,
        rates: [0; 12],
        rates_len: 0,
        unsupported_basic_rate: false,
        rsn: None,
    };
    let mut input = Cursor(&frame[36..]);
    let mut ssid = false;
    let (mut ds_channel, mut ht_channel) = (None, None);
    while !input.0.is_empty() {
        let header = input.take(2)?;
        let element = input.take(header[1] as usize)?;
        match header[0] {
            1 | 50 => {
                if element.is_empty() || (header[0] == 1 && element.len() > 8) {
                    return Err(Errno::EIO);
                }
                for &rate in element {
                    if ![2, 4, 11, 22, 12, 18, 24, 36, 48, 72, 96, 108].contains(&(rate & 0x7f)) {
                        result.unsupported_basic_rate |= rate & 0x80 != 0;
                        continue;
                    }
                    if let Some(old) = result.rates[..result.rates_len as usize]
                        .iter_mut()
                        .find(|old| **old & 0x7f == rate & 0x7f)
                    {
                        *old |= rate & 0x80;
                    } else if result.rates_len < 12 {
                        result.rates[result.rates_len as usize] = rate;
                        result.rates_len += 1;
                    }
                }
            }
            0 => {
                if ssid || element.len() > 32 {
                    return Err(Errno::EIO);
                }
                result.ssid[..element.len()].copy_from_slice(element);
                result.ssid_len = element.len() as u8;
                ssid = true;
            }
            3 => {
                if element.len() != 1 || ds_channel.replace(element[0]).is_some() {
                    return Err(Errno::EIO);
                }
            }
            5 => {
                if element.len() < 4 || element[1] == 0 {
                    return Err(Errno::EIO);
                }
                result.dtim = element[1];
            }
            48 => {
                if result.rsn.is_some() {
                    return Err(Errno::EIO);
                }
                result.rsn = Some(rsn(element)?);
            }
            61 => {
                if element.len() < 22 || ht_channel.replace(element[0]).is_some() {
                    return Err(Errno::EIO);
                }
            }
            _ => {}
        }
    }
    if !ssid || result.beacon_interval == 0 {
        return Err(Errno::EIO);
    }
    if let (Some(ds), Some(ht)) = (ds_channel, ht_channel) {
        if ds != ht {
            return Err(Errno::EIO);
        }
    }
    if let Some(channel) = ds_channel.or(ht_channel) {
        result.frequency = primary_frequency(channel, frequency)?;
    }
    Ok(result)
}
/// The selected association suite: WPA2-Personal, CCMP and no negotiated PMF.
pub const RSN_IE: [u8; 22] = [
    48, 20, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 4, 1, 0, 0, 0x0f, 0xac, 2, 0, 0,
];

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec::Vec;
    fn frame() -> Vec<u8> {
        let mut frame = alloc::vec![0; 36];
        frame[0] = 0x80;
        frame[16..22].copy_from_slice(&[2, 3, 4, 5, 6, 7]);
        frame[32] = 100;
        frame[34] = 0x11;
        frame.extend_from_slice(&[0, 5]);
        frame.extend_from_slice(b"Wi Fi");
        frame.extend_from_slice(&RSN_IE);
        frame
    }
    #[test]
    fn real_beacon_layout_retains_exact_ssid_and_only_accepts_supported_security() {
        let frame = frame();
        let ap = beacon(&frame, 2412, -50).unwrap();
        assert_eq!(ap.ssid(), b"Wi Fi");
        assert_eq!(ap.bssid, [2, 3, 4, 5, 6, 7]);
        assert!(ap.wpa2());
        let mut bad = frame.clone();
        bad[34] &= !0x10;
        assert!(!beacon(&bad, 2412, -50).unwrap().wpa2());
        for end in 0..frame.len() {
            let _ = beacon(&frame[..end], 2412, -50);
        }
        let mut duplicate = frame;
        duplicate.extend_from_slice(&[0, 1, b'x']);
        assert!(beacon(&duplicate, 2412, -50).is_err());
    }
    #[test]
    fn rsn_never_confuses_enterprise_wpa3_tkip_or_required_pmf_with_wpa2_ccmp() {
        assert!(rsn(&RSN_IE[2..]).unwrap().supported());
        for (offset, value) in [(19, 1), (19, 8), (7, 2), (13, 2), (20, 0x40)] {
            let mut ie = RSN_IE;
            ie[offset] = value;
            assert!(!rsn(&ie[2..]).unwrap().supported());
        }
        let mut bad = RSN_IE;
        bad[8] = 255;
        assert!(rsn(&bad[2..]).is_err());
        for end in 0..18 {
            assert!(rsn(&RSN_IE[2..end + 2]).is_err());
        }
    }
    #[test]
    fn advertised_primary_channel_overrides_adjacent_rx_channels_in_beacons_and_probe_responses() {
        for subtype in [0x80, 0x50] {
            for (channel, received, expected) in [
                (3, 2412, 2422),
                (11, 2472, 2462),
                (13, 2462, 2472),
                (14, 2472, 2484),
                (36, 5200, 5180),
                (165, 5805, 5825),
            ] {
                for ds in [false, true] {
                    let mut frame = frame();
                    frame[0] = subtype;
                    if ds {
                        frame.extend_from_slice(&[3, 1, channel]);
                    }
                    frame.extend_from_slice(&[61, 22, channel]);
                    frame.extend_from_slice(&[0; 21]);
                    let ap = beacon(&frame, received, -50).unwrap();
                    assert_eq!(ap.frequency, expected);
                    assert_eq!(ap.rssi, -50);
                }
            }
        }
        let mut ds_only = frame();
        ds_only.extend_from_slice(&[3, 1, 3]);
        assert_eq!(beacon(&ds_only, 2412, -50).unwrap().frequency, 2422);
        assert_eq!(beacon(&frame(), 2462, -50).unwrap().frequency, 2462);
    }
    #[test]
    fn invalid_duplicate_conflicting_or_cross_band_channel_elements_are_rejected() {
        for (received, elements) in [
            (2412, alloc::vec![3, 0]),
            (2412, alloc::vec![3, 2, 1, 1]),
            (2412, alloc::vec![3, 1, 0]),
            (2412, alloc::vec![3, 1, 15]),
            (2412, alloc::vec![3, 1, 36]),
            (5180, alloc::vec![3, 1, 3]),
            (2412, alloc::vec![3, 1, 3, 3, 1, 3]),
            (2412, alloc::vec![61, 0]),
            (2412, alloc::vec![61, 1, 3]),
        ] {
            let mut frame = frame();
            frame.extend_from_slice(&elements);
            assert!(beacon(&frame, received, -50).is_err());
        }
        let mut ht = alloc::vec![61, 22, 3];
        ht.extend_from_slice(&[0; 21]);
        for channel in [0, 15, 36] {
            let mut bad = frame();
            let mut element = ht.clone();
            element[2] = channel;
            bad.extend_from_slice(&element);
            assert!(beacon(&bad, 2412, -50).is_err());
        }
        for elements in [
            [ht.as_slice(), ht.as_slice()].concat(),
            [&[3, 1, 11], ht.as_slice()].concat(),
        ] {
            let mut bad = frame();
            bad.extend_from_slice(&elements);
            assert!(beacon(&bad, 2412, -50).is_err());
        }
        for length in 0..ht.len() {
            let mut bad = frame();
            bad.extend_from_slice(&ht[..length]);
            if length != 0 {
                assert!(beacon(&bad, 2412, -50).is_err());
            }
        }
    }
}
