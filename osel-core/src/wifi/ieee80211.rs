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
        rsn: None,
    };
    let mut input = Cursor(&frame[36..]);
    let mut ssid = false;
    while !input.0.is_empty() {
        let header = input.take(2)?;
        let element = input.take(header[1] as usize)?;
        match header[0] {
            0 => {
                if ssid || element.len() > 32 {
                    return Err(Errno::EIO);
                }
                result.ssid[..element.len()].copy_from_slice(element);
                result.ssid_len = element.len() as u8;
                ssid = true;
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
            _ => {}
        }
    }
    if !ssid || result.beacon_interval == 0 {
        return Err(Errno::EIO);
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
}
