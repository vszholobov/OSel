// SPDX-License-Identifier: ISC
// WMI TLV ABI follows Linux ath10k v6.12; see firmware/ath10k-driver-ISC.txt.
// Copyright (c) 2005-2011 Atheros Communications Inc.
// Copyright (c) 2011-2017 Qualcomm Atheros, Inc.
// Regulatory mapping follows ath/regd_common.h (ISC), Atheros 2008-2009.
//! Station-only WMI commands. No secret command implements Debug.
use super::{Elements, put_tlv, unique, word, words};
use crate::{
    errno::{Errno, KResult},
    wifi::{ieee80211::AccessPoint, station},
};
use alloc::vec::Vec;
use zeroize::Zeroizing;

// 10 dBm, expressed in the 0.5 dBm units used by WMI channel/PDEV limits.
// Below ath's WORC_WORLD channel 1-11 ceiling (20 dBm). Firmware still applies
// the exact board's calibration. SERVICE_READY's raw power fields are not dBm.
pub const TX_POWER_HALF_DBM: u32 = 20;

fn command(id: u32, tag: u16, value: &[u8]) -> KResult<Vec<u8>> {
    let mut result = id.to_le_bytes().to_vec();
    put_tlv(&mut result, tag, value)?;
    Ok(result)
}
fn mac(bytes: [u8; 6]) -> Vec<u8> {
    let mut result = bytes.to_vec();
    result.extend_from_slice(&[0, 0]);
    result
}
pub fn vdev_param(id: u32, value: u32) -> KResult<Vec<u8>> {
    command(0x5008, 95, &words(&[0, id, value]))
}
pub fn tx_encapsulation() -> KResult<Vec<u8>> {
    // ath10k keeps the vdev default in Native WiFi mode even with Ethernet
    // offload. Each HTT descriptor chooses Ethernet or management separately.
    vdev_param(48, 1)
}
/// Match ath10k's lowest basic management rate, independent of IE ordering.
/// If no basic bit was advertised, use the lowest supported legacy rate.
pub fn management_rate(ap: &AccessPoint) -> KResult<u32> {
    station::supported(ap)?;
    let rates = &ap.rates[..usize::from(ap.rates_len)];
    let basic = rates.iter().any(|rate| rate & 0x80 != 0);
    let rate = rates
        .iter()
        .filter(|&&rate| !basic || rate & 0x80 != 0)
        .map(|rate| rate & 0x7f)
        .min()
        .ok_or(Errno::EOPNOTSUPP)?;
    // CCK uses the long preamble. These are hardware codes, not Mbps or the
    // 500 kbit/s values carried by Supported Rates / Extended Supported Rates.
    match rate {
        2 => Ok(0x43),
        4 => Ok(0x42),
        11 => Ok(0x41),
        22 => Ok(0x40),
        12 => Ok(3),
        18 => Ok(7),
        24 => Ok(2),
        36 => Ok(6),
        48 => Ok(1),
        72 => Ok(5),
        96 => Ok(0),
        108 => Ok(4),
        _ => Err(Errno::EOPNOTSUPP),
    }
}
fn home_channel(ap: &AccessPoint) -> Vec<u8> {
    // min/max/regulatory power, regulatory class; antenna gain/max TX power.
    // Both channel publication and VDEV_START must carry the same limits.
    words(&[
        u32::from(ap.frequency),
        u32::from(ap.frequency),
        0,
        1,
        TX_POWER_HALF_DBM << 8 | TX_POWER_HALF_DBM << 16,
        TX_POWER_HALF_DBM << 8,
    ])
}
/// Configure a started legacy STA before creating its AP peer or sending auth.
/// Firmware defaults are not a substitute for programming contention and wake
/// policy. Keep QoS/U-APSD disabled, and keep the station awake during WPA2/data.
pub fn prepare_authentication(mut send: impl FnMut(&[u8]) -> KResult<()>) -> KResult<()> {
    let mut queues = words(&[0]); // vdev 0
    for _ in 0..4 {
        // BE, BK, VI, VO: reserved word then wmi_wmm_params. Legacy 11g DCF.
        queues.extend_from_slice(&words(&[0, 15, 1023, 2, 0, 0, 0]));
    }
    send(&command(0x500d, 199, &queues)?)?;
    for (id, value) in [(0, 0), (1, 1), (2, 0), (4, 0)] {
        // RX_WAKE=WAKE, TX_WAKE=ALWAYS, PSPOLL=NO_MAX, UAPSD=0.
        send(&command(0x9002, 109, &words(&[0, id, value]))?)?;
    }
    send(&command(0x9001, 108, &words(&[0, 0]))?) // STA_PS_MODE_DISABLED
}
/// Read-only PDEV counters; never request peer stats or receive payloads.
pub fn request_tx_stats() -> KResult<Vec<u8>> {
    command(0x16001, 143, &words(&[4, 0, 0, 0, 0]))
}
#[derive(Debug, PartialEq, Eq)]
pub struct TxStats {
    pub power: u32,
    pub counters: [u32; 15],
}
/// The TLV ABI uses the original 180-byte wmi_pdev_stats, not the 10.4 layout.
/// A firmware response can omit PDEV stats; wait for a later response then.
pub fn tx_stats(data: &[u8]) -> KResult<Option<TxStats>> {
    if word(data, 0)? & 0xffffff != 0x1d004 {
        return Err(Errno::EIO);
    }
    let mut header = None;
    let mut stats = None;
    let mut elements = Elements::new(&data[4..]);
    while let Some((tag, value)) = elements.next()? {
        match tag {
            70 => unique(&mut header, value)?,
            17 => unique(&mut stats, value)?,
            _ => (),
        }
    }
    let count = word(header.ok_or(Errno::EIO)?, 1)?;
    let stats = stats.ok_or(Errno::EIO)?;
    if count == 0 {
        return Ok(None);
    }
    if count != 1 || stats.len() < 180 {
        return Err(Errno::EIO);
    }
    let mut counters = [0; 15];
    for (value, index) in counters
        .iter_mut()
        .zip([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 12, 16, 20, 22])
    {
        *value = word(stats, 7 + index)?;
    }
    Ok(Some(TxStats {
        power: word(stats, 6)?,
        counters,
    }))
}
pub fn authorize(ap: [u8; 6]) -> KResult<Vec<u8>> {
    peer_param(ap, 3, 1)
}
pub fn wake(ap: [u8; 6]) -> KResult<Vec<u8>> {
    peer_param(ap, 0xff, 1)
}
pub fn peer_param(ap: [u8; 6], id: u32, value: u32) -> KResult<Vec<u8>> {
    let mut body = words(&[0]);
    body.extend_from_slice(&mac(ap));
    body.extend_from_slice(&words(&[id, value]));
    command(0x6004, 100, &body)
}
pub fn create_peer(ap: [u8; 6]) -> KResult<Vec<u8>> {
    let mut body = words(&[0]);
    body.extend_from_slice(&mac(ap));
    body.extend_from_slice(&words(&[0])); // WMI_PEER_TYPE_DEFAULT, as STA_NONE in ath10k
    command(0x6001, 97, &body)
}
/// Every firmware boot needs its own channel table, including a fresh connect
/// after scanning stopped/reset the target. Publish only the admitted home
/// channel, configure its verified EEPROM domain, then start the vdev with the
/// same 11g/20 MHz and 10 dBm limits. Set the PDEV power ceiling and AP's basic
/// management rate before start/auth. Unknown domains must not become WORC_WORLD.
pub fn start_with_channel(
    ap: &AccessPoint,
    eeprom_domain: u32,
    mut send: impl FnMut(&[u8]) -> KResult<()>,
) -> KResult<()> {
    station::supported(ap)?;
    let domain = regdomain(eeprom_domain)?;
    let rate = management_rate(ap)?;
    let mut channels = Vec::new();
    put_tlv(&mut channels, 80, &home_channel(ap))?;
    let mut list = command(0x3003, 79, &words(&[1]))?;
    put_tlv(&mut list, 18, &channels)?;
    send(&list)?;
    send(&domain)?;
    send(&command(0x4003, 82, &words(&[3, TX_POWER_HALF_DBM]))?)?;
    send(&vdev_param(25, rate)?)?;
    send(&start(ap)?)
}
pub fn regdomain(eeprom_domain: u32) -> KResult<Vec<u8>> {
    // ath_regd_get_eepromRD strips only WORLDWIDE_ROAMING_FLAG. This Acer
    // reports WORC_WORLD; its regDomainPairs entry has NO_CTL for both bands.
    // Keep country-coded and other, unaudited EEPROM domains unsupported.
    let domain = eeprom_domain & !0x4000;
    if domain != 0x6c {
        return Err(Errno::EOPNOTSUPP);
    }
    command(0x4001, 81, &words(&[0, domain, domain, domain, 0xff, 0xff]))
}
pub fn start(ap: &AccessPoint) -> KResult<Vec<u8>> {
    station::supported(ap)?;
    let mut body = words(&[
        0,
        0,
        ap.beacon_interval as u32,
        ap.dtim as u32,
        0,
        ap.ssid_len as u32,
    ]);
    body.extend_from_slice(&ap.ssid);
    body.extend_from_slice(&words(&[0, 0, 0, 0]));
    let mut result = command(0x5003, 88, &body)?;
    // Legacy 11g/20 MHz; no HT/VHT or DFS.
    put_tlv(&mut result, 80, &home_channel(ap))?;
    put_tlv(&mut result, 18, &[])?;
    Ok(result)
}
pub fn started(data: &[u8]) -> KResult<()> {
    if word(data, 0)? & 0xffffff != 0x5001 {
        return Err(Errno::EIO);
    }
    let mut response = None;
    let mut elements = Elements::new(&data[4..]);
    while let Some((tag, value)) = elements.next()? {
        if tag == 40 {
            unique(&mut response, value)?;
        }
    }
    let response = response.ok_or(Errno::EIO)?;
    if word(response, 0)? != 0
        || word(response, 1)? != 0
        || word(response, 2)? != 0
        || word(response, 3)? != 0
    {
        return Err(Errno::EIO);
    }
    Ok(())
}
pub fn lost(data: &[u8], ap: [u8; 6]) -> KResult<bool> {
    let id = word(data, 0)? & 0xffffff;
    if id != 0x6001 && id != 0xb001 {
        return Ok(false);
    }
    let mut response = None;
    let mut elements = Elements::new(&data[4..]);
    let expected = if id == 0x6001 { 43 } else { 48 };
    while let Some((tag, value)) = elements.next()? {
        if tag == expected {
            unique(&mut response, value)?;
        }
    }
    let value = response.ok_or(Errno::EIO)?;
    if id == 0x6001 {
        return Ok(value.get(..6).ok_or(Errno::EIO)? == ap);
    }
    Ok(word(value, 0)? == 0 && matches!(word(value, 1)?, 2 | 5))
}
pub fn associated(ap: &AccessPoint, aid: u16) -> KResult<Vec<u8>> {
    station::supported(ap)?;
    if !(1..=2007).contains(&aid) {
        return Err(Errno::EINVAL);
    }
    let mut body = mac(ap.bssid);
    body.extend_from_slice(&words(&[
        0,
        1,
        aid as u32,
        5,
        ap.capabilities as u32,
        1,
        0,
        0,
        0,
        0,
        1,
        0,
        1,
        0,
        0,
        ap.rates_len as u32,
        0,
    ]));
    let mut result = command(0x6005, 101, &body)?;
    let mut rates: Vec<u8> = ap.rates[..ap.rates_len as usize]
        .iter()
        .map(|rate| rate & 0x7f)
        .collect();
    while rates.len() % 4 != 0 {
        rates.push(0);
    }
    put_tlv(&mut result, 17, &rates)?;
    put_tlv(&mut result, 17, &[])?;
    put_tlv(&mut result, 102, &words(&[0; 4]))?;
    Ok(result)
}
pub fn up(ap: [u8; 6], aid: u16) -> KResult<Vec<u8>> {
    if !(1..=2007).contains(&aid) {
        return Err(Errno::EINVAL);
    }
    let mut body = words(&[0, aid as u32]);
    body.extend_from_slice(&mac(ap));
    command(0x5005, 92, &body)
}
pub fn key(
    ap: [u8; 6],
    group: bool,
    index: u8,
    rsc: [u8; 8],
    value: &[u8; 16],
) -> KResult<Zeroizing<Vec<u8>>> {
    if index > 3 || (!group && index != 0) || rsc[6..] != [0; 2] {
        return Err(Errno::EINVAL);
    }
    let mut body = Zeroizing::new(words(&[0]));
    body.extend_from_slice(&mac(ap));
    body.extend_from_slice(&words(&[index as u32, u32::from(group), 4])); // CCMP
    body.extend_from_slice(&rsc);
    body.extend_from_slice(&[0; 48]);
    body.extend_from_slice(&words(&[16, 0, 0]));
    let mut result = Zeroizing::new(command(0x5009, 96, &body)?);
    put_tlv(&mut result, 17, value)?;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ccmp_key_command_keeps_counters_and_secret_out_of_debug() {
        let result = key(
            [2, 3, 4, 5, 6, 7],
            true,
            2,
            [1, 2, 3, 4, 5, 6, 0, 0],
            &[0xaa; 16],
        )
        .unwrap();
        assert_eq!(word(&result, 0).unwrap(), 0x5009);
        let mut elements = Elements::new(&result[4..]);
        let (tag, body) = elements.next().unwrap().unwrap();
        assert_eq!(tag, 96);
        assert_eq!(body.len(), 92);
        assert_eq!(&body[24..32], &[1, 2, 3, 4, 5, 6, 0, 0]);
        assert_eq!(word(body, 20).unwrap(), 16);
        assert_eq!(elements.next().unwrap(), Some((17, &[0xaa; 16][..])));
        assert!(key([2; 6], false, 1, [0; 8], &[0; 16]).is_err());
    }
    #[test]
    fn beacon_loss_and_kickout_are_bound_to_the_current_station() {
        let roam = command(0xb001, 48, &words(&[0, 2, 0])).unwrap();
        assert!(lost(&roam, [2; 6]).unwrap());
        let other = command(0xb001, 48, &words(&[1, 2, 0])).unwrap();
        assert!(!lost(&other, [2; 6]).unwrap());
        let rssi = command(0xb001, 48, &words(&[0, 3, 0])).unwrap();
        assert!(!lost(&rssi, [2; 6]).unwrap());
        let kickout = command(0x6001, 43, &mac([2; 6])).unwrap();
        assert!(lost(&kickout, [2; 6]).unwrap());
        assert!(!lost(&kickout, [4; 6]).unwrap());
        for end in 0..roam.len() {
            assert!(lost(&roam[..end], [2; 6]).is_err());
        }
    }
}
