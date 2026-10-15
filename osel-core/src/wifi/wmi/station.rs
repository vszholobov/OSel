// SPDX-License-Identifier: ISC
// WMI TLV ABI follows Linux ath10k v6.12; see firmware/ath10k-driver-ISC.txt.
// Copyright (c) 2005-2011 Atheros Communications Inc.
// Copyright (c) 2011-2017 Qualcomm Atheros, Inc.
//! Station-only WMI commands. No secret command implements Debug.
use super::{Elements, put_tlv, unique, word, words};
use crate::{
    errno::{Errno, KResult},
    wifi::{ieee80211::AccessPoint, station},
};
use alloc::vec::Vec;
use zeroize::Zeroizing;

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
pub fn ethernet_tx() -> KResult<Vec<u8>> {
    vdev_param(48, 2)
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
    body.extend_from_slice(&words(&[1]));
    command(0x6001, 97, &body)
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
    // Legacy 11g/20 MHz, conservative 0 dBm power limits; no HT/VHT or DFS.
    put_tlv(
        &mut result,
        80,
        &words(&[ap.frequency as u32, ap.frequency as u32, 0, 1, 0, 0]),
    )?;
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
