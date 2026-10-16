// SPDX-License-Identifier: ISC
// WMI TLV ABI adapted from Linux ath10k v6.12 (wmi-tlv.h/c, wmi.h).
// Copyright (c) 2005-2011 Atheros Communications Inc.
// Copyright (c) 2011-2017 Qualcomm Atheros, Inc.
// Copyright (c) 2018-2019 The Linux Foundation. All rights reserved.
// Copyright (c) 2024 Qualcomm Innovation Center, Inc. All rights reserved.
// See firmware/ath10k-driver-ISC.txt.
//! Checked WMI boot events. Target-provided sizes are never allocation authority.
use crate::errno::{Errno, KResult};
use alloc::vec::Vec;
pub mod station;

pub const ABI: [u32; 6] = [0x01000000, 53, 0x5f414351, 0x4c4d, 0, 0];
pub const VDEVS: u32 = 4;
pub const PEERS: u32 = 33;
pub const MSDUS: u32 = 64;
pub const RX_FULL_REORDER: usize = 65;
pub const MGMT_TX_HTT: usize = 75;
pub const MGMT_TX_WMI: usize = 76;
pub struct Elements<'a>(&'a [u8]);
impl<'a> Elements<'a> {
    pub fn new(bytes: &'a [u8]) -> Self {
        Self(bytes)
    }
    pub fn next(&mut self) -> KResult<Option<(u16, &'a [u8])>> {
        if self.0.is_empty() {
            return Ok(None);
        }
        let header = self.0.get(..4).ok_or(Errno::EIO)?;
        let length = u16::from_le_bytes([header[0], header[1]]) as usize;
        let tag = u16::from_le_bytes([header[2], header[3]]);
        let value = self.0.get(4..4 + length).ok_or(Errno::EIO)?;
        self.0 = &self.0[4 + length..];
        Ok(Some((tag, value)))
    }
}
fn word(data: &[u8], index: usize) -> KResult<u32> {
    let bytes = data.get(index * 4..index * 4 + 4).ok_or(Errno::EIO)?;
    Ok(u32::from_le_bytes(bytes.try_into().unwrap()))
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryRequest {
    pub id: u32,
    pub unit_size: u32,
    pub flags: u32,
    pub units: u32,
}
impl MemoryRequest {
    pub fn bytes(&self) -> KResult<usize> {
        let units = if self.flags & 6 != 0 {
            PEERS + 1
        } else if self.flags & 1 != 0 {
            VDEVS + 1
        } else {
            self.units
        };
        let unit = self.unit_size.checked_add(3).ok_or(Errno::EIO)? & !3;
        let length = unit.checked_mul(units).ok_or(Errno::EIO)?;
        if length == 0 || length > 1024 * 1024 {
            return Err(Errno::EIO);
        }
        Ok(length as usize)
    }
}
#[derive(Debug, PartialEq, Eq)]
pub struct ServiceReady {
    pub build: u32,
    pub abi: [u32; 6],
    pub chains: u32,
    pub tx_power: [u32; 2],
    pub eeprom_domain: u32,
    pub channels_2ghz: [u32; 2],
    pub channels_5ghz: [u32; 2],
    pub services: Vec<u32>,
    pub requests: Vec<MemoryRequest>,
}
impl ServiceReady {
    pub fn supports(&self, id: usize) -> bool {
        // ath10k's initial bitmap uses four bits per u32; the TLV extension uses
        // the dense layout. The boundary is the original bitmap byte length.
        let length = self.services.len() * 4;
        let (word, bit) = if id < length {
            (id / 4, id % 4)
        } else if id < 256 {
            ((id - length) / 32, (id - length) % 32)
        } else {
            return false;
        };
        self.services
            .get(word)
            .is_some_and(|value| value & (1 << bit) != 0)
    }
}
#[derive(Debug, Clone, Copy)]
pub struct MemoryChunk {
    pub id: u32,
    pub address: u32,
    pub length: u32,
}
pub fn put_tlv(output: &mut Vec<u8>, tag: u16, data: &[u8]) -> KResult<()> {
    let length = u16::try_from(data.len()).map_err(|_| Errno::E2BIG)?;
    output.extend_from_slice(&length.to_le_bytes());
    output.extend_from_slice(&tag.to_le_bytes());
    output.extend_from_slice(data);
    Ok(())
}
fn words(words: &[u32]) -> Vec<u8> {
    words.iter().flat_map(|word| word.to_le_bytes()).collect()
}
pub fn init(service: &ServiceReady, chunks: &[MemoryChunk]) -> KResult<Vec<u8>> {
    if service.chains == 0 || service.chains > 4 {
        return Err(Errno::EINVAL);
    }
    if chunks.len() != service.requests.len() {
        return Err(Errno::EINVAL);
    }
    let mut output = 1u32.to_le_bytes().to_vec();
    let mut header = words(&ABI);
    header.extend_from_slice(&(chunks.len() as u32).to_le_bytes());
    put_tlv(&mut output, 74, &header)?;
    let mut resources = [0u32; 44];
    resources[0] = VDEVS;
    resources[1] = PEERS;
    if service.supports(RX_FULL_REORDER) {
        resources[2] = VDEVS;
        resources[3] = VDEVS;
    }
    resources[4] = 2;
    resources[5] = PEERS * 2;
    resources[6] = 16;
    resources[7] = (1 << service.chains) - 1;
    resources[8] = resources[7];
    resources[9..13].copy_from_slice(&[100, 100, 100, 40]);
    resources[13] = 2; // Ethernet II RX decapsulation
    resources[14] = 4;
    resources[15] = VDEVS;
    resources[16] = VDEVS;
    resources[17] = 8;
    resources[21] = 0x400;
    resources[22] = 32;
    resources[27] = 2;
    resources[28] = MSDUS;
    resources[29] = 2;
    // No TDLS, AP beacon offload or RX bundles are requested by this station driver.
    resources[33] = 5;
    resources[34] = 22;
    resources[35] = 6;
    resources[37] = 1;
    resources[38] = 1;
    put_tlv(&mut output, 75, &words(&resources))?;
    let mut memory = Vec::new();
    for (request, chunk) in service.requests.iter().zip(chunks) {
        if chunk.id != request.id
            || chunk.length as usize != request.bytes()?
            || chunk.address == 0
            || chunk.address & 3 != 0
            || chunk.address.checked_add(chunk.length).is_none()
        {
            return Err(Errno::EINVAL);
        }
        put_tlv(
            &mut memory,
            76,
            &words(&[chunk.id, chunk.address, chunk.length, 0]),
        )?;
    }
    put_tlv(&mut output, 18, &memory)?;
    Ok(output)
}
pub fn ready(data: &[u8]) -> KResult<[u8; 6]> {
    if word(data, 0)? & 0xffffff != 2 {
        return Err(Errno::EIO);
    }
    let mut ready = None;
    let mut elements = Elements::new(&data[4..]);
    while let Some((tag, value)) = elements.next()? {
        if tag == 35 {
            unique(&mut ready, value)?;
        }
    }
    let ready = ready.filter(|data| data.len() >= 36).ok_or(Errno::EIO)?;
    for index in [0, 2, 3, 4, 5] {
        if word(ready, index)? != ABI[index] {
            return Err(Errno::EOPNOTSUPP);
        }
    }
    if word(ready, 8)? != 0 {
        return Err(Errno::EIO);
    }
    let mac: [u8; 6] = ready[24..30].try_into().unwrap();
    if mac == [0; 6] || mac[0] & 1 != 0 {
        return Err(Errno::EIO);
    }
    Ok(mac)
}
pub fn vdev_create(mac: [u8; 6]) -> KResult<Vec<u8>> {
    if mac == [0; 6] || mac[0] & 1 != 0 {
        return Err(Errno::EINVAL);
    }
    let mut command = 0x5001u32.to_le_bytes().to_vec();
    let mut body = words(&[0, 2, 0]); // vdev zero, station, no subtype
    body.extend_from_slice(&mac);
    body.extend_from_slice(&[0; 2]);
    put_tlv(&mut command, 86, &body)?;
    Ok(command)
}
pub fn passive_channels(service: &ServiceReady) -> Vec<u32> {
    passive_frequencies(service.channels_2ghz, service.channels_5ghz)
}
/// Receive-only discovery. Extra channels do not authorize station TX there.
/// Avoid DFS channels; keep every frequency within the board's advertised limits.
pub fn passive_frequencies(band2: [u32; 2], band5: [u32; 2]) -> Vec<u32> {
    (1..=13)
        .map(|channel| 2407 + 5 * channel)
        .filter(|frequency| *frequency >= band2[0] && *frequency <= band2[1])
        .chain(
            [5180, 5200, 5220, 5240, 5745, 5765, 5785, 5805, 5825]
                .into_iter()
                .filter(|frequency| *frequency >= band5[0] && *frequency <= band5[1]),
        )
        .collect()
}
pub fn scan_channels(frequencies: &[u32]) -> KResult<Vec<u8>> {
    if frequencies.is_empty() || frequencies.len() > 22 {
        return Err(Errno::EINVAL);
    }
    let mut command = 0x3003u32.to_le_bytes().to_vec();
    put_tlv(&mut command, 79, &words(&[frequencies.len() as u32]))?;
    let mut channels = Vec::new();
    for &frequency in frequencies {
        let band2 = (2412..=2472).contains(&frequency) && (frequency - 2412) % 5 == 0;
        let band5 = [5180, 5200, 5220, 5240, 5745, 5765, 5785, 5805, 5825].contains(&frequency);
        if !band2 && !band5 {
            return Err(Errno::EINVAL);
        }
        // Receive-only 11g/11a, 20 MHz. No TX power is requested.
        put_tlv(
            &mut channels,
            80,
            &words(&[frequency, frequency, 0, u32::from(band2) | (1 << 7), 0, 0]),
        )?;
    }
    put_tlv(&mut command, 18, &channels)?;
    Ok(command)
}
pub fn start_scan(frequencies: &[u32]) -> KResult<Vec<u8>> {
    start_scan_id(frequencies, 0xa001)
}
pub fn start_scan_id(frequencies: &[u32], id: u32) -> KResult<Vec<u8>> {
    let _ = scan_channels(frequencies)?;
    if !(0xa001..=0xafff).contains(&id) {
        return Err(Errno::EINVAL);
    }
    let mut common = [0u32; 25];
    common[0] = id;
    common[1] = 0xa001;
    common[4] = 0x14b; // started, completed, foreign channel/exit, start failed
    common[5] = 50;
    common[6] = 200;
    common[7] = 50;
    common[8] = 500;
    common[12] = 10000;
    common[14] = 0x21; // passive, ath10k TLV inverts FILTER_PROBE_REQ
    common[16] = frequencies.len() as u32;
    common[17] = 1;
    common[20] = 3;
    let mut command = 0x3001u32.to_le_bytes().to_vec();
    put_tlv(&mut command, 77, &words(&common))?;
    put_tlv(&mut command, 16, &words(frequencies))?;
    put_tlv(&mut command, 19, &[])?; // wildcard SSID
    put_tlv(
        &mut command,
        19,
        &[0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0, 0],
    )?;
    put_tlv(&mut command, 17, &[])?;
    Ok(command)
}
pub struct Management<'a> {
    pub frame: &'a [u8],
    pub frequency: u16,
    pub rssi: i32,
}
pub fn management(data: &[u8]) -> KResult<Management<'_>> {
    if word(data, 0)? & 0xffffff != 0x7001 {
        return Err(Errno::EIO);
    }
    let (mut header, mut frame) = (None, None);
    let mut elements = Elements::new(&data[4..]);
    while let Some((tag, value)) = elements.next()? {
        match tag {
            44 => unique(&mut header, value)?,
            17 => unique(&mut frame, value)?,
            _ => {}
        }
    }
    let header = header.filter(|data| data.len() >= 40).ok_or(Errno::EIO)?;
    let frame = frame
        .ok_or(Errno::EIO)?
        .get(..word(header, 4)? as usize)
        .ok_or(Errno::EIO)?;
    if word(header, 5)? & 0x39 != 0 {
        return Err(Errno::EIO);
    }
    let frequency = match word(header, 0)? {
        14 => 2484,
        channel @ 1..=13 => 2407 + channel * 5,
        channel @ 36..=196 => 5000 + channel * 5,
        _ => return Err(Errno::EIO),
    } as u16;
    let snr = word(header, 1)? as i32;
    if !(-32..=127).contains(&snr) {
        return Err(Errno::EIO);
    }
    Ok(Management {
        frame,
        frequency,
        rssi: (snr - 95).clamp(-127, 0),
    })
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ScanEvent {
    pub kind: u32,
    pub reason: u32,
    pub frequency: u32,
}
pub fn scan_event(data: &[u8], id: u32) -> KResult<ScanEvent> {
    if word(data, 0)? & 0xffffff != 0x3001 {
        return Err(Errno::EIO);
    }
    let mut scan = None;
    let mut elements = Elements::new(&data[4..]);
    while let Some((tag, value)) = elements.next()? {
        if tag == 36 {
            unique(&mut scan, value)?;
        }
    }
    let scan = scan.filter(|data| data.len() >= 24).ok_or(Errno::EIO)?;
    if word(scan, 3)? != 0xa001 || word(scan, 4)? != id || word(scan, 5)? != 0 {
        return Err(Errno::EIO);
    }
    let kind = word(scan, 0)?;
    if kind & 0x40 != 0 {
        return Err(Errno::EIO);
    }
    if kind & 2 != 0 && word(scan, 1)? != 0 {
        return Err(Errno::EIO);
    }
    Ok(ScanEvent {
        kind,
        reason: word(scan, 1)?,
        frequency: word(scan, 2)?,
    })
}
pub fn scan_complete(data: &[u8]) -> KResult<bool> {
    Ok(scan_event(data, 0xa001)?.kind & 2 != 0)
}
/// Accumulate all passes without erasing APs missed in a later pass. Counters
/// distinguish no radio frames from malformed metadata or rejected beacons.
#[derive(Default)]
pub struct ScanResults {
    pub networks: [Option<super::ieee80211::AccessPoint>; 20],
    pub events: u32,
    pub channels: u32,
    pub management: u32,
    pub bad_management: u32,
    pub bad_beacons: u32,
    pub beacons: u32,
}
impl ScanResults {
    pub fn event(&mut self, data: &[u8], id: u32) -> KResult<bool> {
        self.events += 1;
        match word(data, 0)? & 0xffffff {
            0x3001 => {
                let scan = scan_event(data, id)?;
                self.channels += u32::from(scan.kind & 8 != 0);
                return Ok(scan.kind & 2 != 0);
            }
            0x7001 => {
                self.management += 1;
                let frame = match management(data) {
                    Ok(frame) => frame,
                    Err(_) => {
                        self.bad_management += 1;
                        return Ok(false);
                    }
                };
                if !matches!(frame.frame.first(), Some(0x80 | 0x50)) {
                    return Ok(false);
                }
                match super::ieee80211::beacon(frame.frame, frame.frequency, frame.rssi) {
                    Ok(ap) => {
                        self.beacons += 1;
                        let index = self
                            .networks
                            .iter()
                            .position(|old| old.is_some_and(|old| old.bssid == ap.bssid))
                            .or_else(|| self.networks.iter().position(Option::is_none));
                        if let Some(index) = index {
                            self.networks[index] = Some(ap);
                        }
                    }
                    Err(_) => self.bad_beacons += 1,
                }
            }
            _ => (),
        }
        Ok(false)
    }
}
fn unique<'a>(slot: &mut Option<&'a [u8]>, value: &'a [u8]) -> KResult<()> {
    if slot.replace(value).is_some() {
        return Err(Errno::EIO);
    }
    Ok(())
}
/// Wait through optional SERVICE_AVAILABLE events before SERVICE_READY.
/// The transport must enforce one deadline across all receive calls.
pub fn wait_service_ready(mut receive: impl FnMut() -> KResult<Vec<u8>>) -> KResult<ServiceReady> {
    for _ in 0..32 {
        let data = receive()?;
        match word(&data, 0)? & 0xffffff {
            1 => return service_ready(&data),
            3 => {
                // ath10k sends the extended service bitmap before SERVICE_READY.
                // This station uses only base services; do not treat the bitmap
                // as readiness or use it to bypass the full boot validation.
                let mut available = None;
                let mut elements = Elements::new(&data[4..]);
                while let Some((tag, value)) = elements.next()? {
                    if tag == 0x22f {
                        unique(&mut available, value)?;
                    }
                }
                // Segment offset plus four bitmap words in the bundled ABI.
                if available.is_none_or(|value| value.len() != 20) {
                    return Err(Errno::EIO);
                }
            }
            _ => return Err(Errno::EIO),
        }
    }
    Err(Errno::ETIMEDOUT)
}
pub fn service_ready(data: &[u8]) -> KResult<ServiceReady> {
    if word(data, 0)? & 0xffffff != 1 {
        return Err(Errno::EIO);
    }
    let (mut event, mut reg, mut services, mut requests) = (None, None, None, None);
    let mut elements = Elements::new(&data[4..]);
    while let Some((tag, value)) = elements.next()? {
        match tag {
            32 => unique(&mut event, value)?,
            33 => unique(&mut reg, value)?,
            16 if services.is_none() => services = Some(value),
            18 => unique(&mut requests, value)?,
            _ => {}
        }
    }
    let event = event.filter(|b| b.len() >= 104).ok_or(Errno::EIO)?;
    let reg = reg.filter(|b| b.len() >= 36).ok_or(Errno::EIO)?;
    let services = services
        .filter(|b| !b.is_empty() && b.len() % 4 == 0 && b.len() <= 512)
        .ok_or(Errno::EIO)?;
    let requests = requests.ok_or(Errno::EIO)?;
    let abi: [u32; 6] = core::array::from_fn(|i| word(event, i + 1).unwrap());
    for index in [0, 2, 3, 4, 5] {
        if abi[index] != ABI[index] {
            return Err(Errno::EOPNOTSUPP);
        }
    }
    let chains = word(event, 9)?;
    if chains == 0 || chains > 4 || word(event, 18)? > 16 {
        return Err(Errno::EIO);
    }
    let mut memory = Vec::new();
    let mut elements = Elements::new(requests);
    while let Some((tag, value)) = elements.next()? {
        if tag != 34 || value.len() != 16 || memory.len() >= 16 {
            return Err(Errno::EIO);
        }
        let request = MemoryRequest {
            id: word(value, 0)?,
            unit_size: word(value, 1)?,
            flags: word(value, 2)?,
            units: word(value, 3)?,
        };
        if request.unit_size == 0
            || request.unit_size > 1024 * 1024
            || request.flags & !7 != 0
            || memory.iter().any(|r: &MemoryRequest| r.id == request.id)
        {
            return Err(Errno::EIO);
        }
        memory.push(request);
    }
    if memory.len() != word(event, 18)? as usize {
        return Err(Errno::EIO);
    }
    Ok(ServiceReady {
        build: word(event, 0)?,
        abi,
        chains,
        tx_power: [word(event, 13)?, word(event, 14)?],
        eeprom_domain: word(reg, 0)?,
        channels_2ghz: [word(reg, 5)?, word(reg, 6)?],
        channels_5ghz: [word(reg, 7)?, word(reg, 8)?],
        services: services
            .chunks_exact(4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()))
            .collect(),
        requests: memory,
    })
}
#[cfg(test)]
mod tests {
    use super::*;
    fn tlv(out: &mut Vec<u8>, tag: u16, data: &[u8]) {
        out.extend_from_slice(&(data.len() as u16).to_le_bytes());
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(data);
    }
    fn event() -> Vec<u8> {
        let mut event = [0u32; 26];
        event[1..7].copy_from_slice(&ABI);
        event[9] = 2;
        event[18] = 1;
        let bytes = |words: &[u32]| {
            words
                .iter()
                .flat_map(|w| w.to_le_bytes())
                .collect::<Vec<_>>()
        };
        let mut out = 1u32.to_le_bytes().to_vec();
        tlv(&mut out, 32, &bytes(&event));
        tlv(
            &mut out,
            33,
            &bytes(&[0x60, 0, 0, 0, 0, 2412, 2472, 5180, 5825]),
        );
        tlv(&mut out, 16, &bytes(&[0x400, 0, 0, 0]));
        let mut requests = Vec::new();
        tlv(&mut requests, 34, &bytes(&[1, 32, 2, 0]));
        tlv(&mut out, 18, &requests);
        out
    }
    fn available() -> Vec<u8> {
        // Acer photo: 28-byte event, ID 3, TLV 022f0014, then 0, 08001000.
        // The remaining bitmap words are zero fixtures, not captured values.
        words(&[3, 0x022f0014, 0, 0x08001000, 0, 0, 0])
    }
    #[test]
    fn acer_service_available_precedes_service_ready() {
        for prelude in 0..=2 {
            let mut received = 0;
            let service = wait_service_ready(|| {
                received += 1;
                Ok(if received <= prelude {
                    available()
                } else {
                    event()
                })
            })
            .unwrap();
            assert_eq!(received, prelude + 1);
            assert_eq!(service, service_ready(&event()).unwrap());
            assert_eq!(service_ready(&available()), Err(Errno::EIO));
        }
    }
    #[test]
    fn service_available_does_not_replace_ready_or_hide_transport_failure() {
        let mut received = 0;
        assert_eq!(
            wait_service_ready(|| {
                received += 1;
                if received == 1 {
                    Ok(available())
                } else {
                    Err(Errno::EINTR)
                }
            }),
            Err(Errno::EINTR)
        );
        assert_eq!(received, 2);
        received = 0;
        assert_eq!(
            wait_service_ready(|| {
                received += 1;
                Ok(available())
            }),
            Err(Errno::ETIMEDOUT)
        );
        assert_eq!(received, 32);
    }
    #[test]
    fn malformed_service_available_is_rejected_before_readiness() {
        let bytes = available();
        for end in 0..bytes.len() {
            let mut received = 0;
            assert_eq!(
                wait_service_ready(|| {
                    received += 1;
                    Ok(bytes[..end].to_vec())
                }),
                Err(Errno::EIO),
                "end={end}"
            );
            assert_eq!(received, 1);
        }
        let mut duplicate = bytes.clone();
        duplicate.extend_from_slice(&bytes[4..]);
        let mut wrong_tag = bytes.clone();
        wrong_tag[6] = 0;
        let mut unknown = bytes.clone();
        unknown[0] = 2;
        let mut short = 3u32.to_le_bytes().to_vec();
        tlv(&mut short, 0x22f, &[0; 16]);
        let mut long = 3u32.to_le_bytes().to_vec();
        tlv(&mut long, 0x22f, &[0; 24]);
        for invalid in [duplicate, wrong_tag, unknown, short, long] {
            assert_eq!(wait_service_ready(|| Ok(invalid.clone())), Err(Errno::EIO));
        }
    }
    #[test]
    fn service_available_does_not_bypass_service_ready_validation() {
        let mut invalid = event();
        invalid[12] = 3;
        let mut packets = [available(), invalid].into_iter();
        assert_eq!(
            wait_service_ready(|| Ok(packets.next().unwrap())),
            Err(Errno::EOPNOTSUPP)
        );
    }
    #[test]
    fn checked_service_ready_retains_abi_regulatory_limits_and_memory_requests() {
        let bytes = event();
        let event = service_ready(&bytes).unwrap();
        assert_eq!(event.abi, ABI);
        assert_eq!(event.chains, 2);
        assert_eq!(event.channels_2ghz, [2412, 2472]);
        assert_eq!(
            event.requests,
            [MemoryRequest {
                id: 1,
                unit_size: 32,
                flags: 2,
                units: 0
            }]
        );
        for end in 0..bytes.len() {
            assert!(service_ready(&bytes[..end]).is_err(), "end={end}");
        }
    }
    #[test]
    fn duplicate_structures_wrong_abi_and_invalid_counts_fail_closed() {
        let mut bytes = event();
        let duplicate = bytes[4..112].to_vec();
        bytes.extend(duplicate);
        assert_eq!(service_ready(&bytes), Err(Errno::EIO));
        let mut bytes = event();
        bytes[12] = 3;
        assert_eq!(service_ready(&bytes), Err(Errno::EOPNOTSUPP));
        let mut bytes = event();
        bytes[8 + 18 * 4] = 17;
        assert_eq!(service_ready(&bytes), Err(Errno::EIO));
        let mut bytes = event();
        bytes[4..6].copy_from_slice(&u16::MAX.to_le_bytes());
        assert_eq!(service_ready(&bytes), Err(Errno::EIO));
    }
    #[test]
    fn firmware_memory_size_matches_resource_counts_and_is_bounded() {
        for (flags, units) in [(0, 3), (1, VDEVS + 1), (2, PEERS + 1), (4, PEERS + 1)] {
            assert_eq!(
                MemoryRequest {
                    id: 1,
                    unit_size: 5,
                    flags,
                    units: 3
                }
                .bytes(),
                Ok(units as usize * 8)
            );
        }
        assert!(
            MemoryRequest {
                id: 0,
                unit_size: u32::MAX,
                flags: 0,
                units: 100
            }
            .bytes()
            .is_err()
        );
        assert!(
            MemoryRequest {
                id: 0,
                unit_size: 100,
                flags: 0,
                units: u32::MAX
            }
            .bytes()
            .is_err()
        );
    }
    #[test]
    fn service_bitmap_uses_four_legacy_bits_per_word_and_dense_extension() {
        let mut service = service_ready(&event()).unwrap();
        service.services = alloc::vec![0; 32];
        service.services[16] = 2;
        assert!(service.supports(RX_FULL_REORDER));
        assert!(!service.supports(64));
        service.services[0] = 1 << 31;
        assert!(!service.supports(31));
        assert!(service.supports(159));
        assert!(!service.supports(256));
    }
    #[test]
    fn init_advertises_only_owned_memory_and_ready_requires_success_and_valid_mac() {
        let service = service_ready(&event()).unwrap();
        let chunk = MemoryChunk {
            id: 1,
            address: 0x1000000,
            length: 32 * (PEERS + 1),
        };
        let packet = init(&service, &[chunk]).unwrap();
        let mut elements = Elements::new(&packet[4..]);
        assert_eq!(elements.next().unwrap().unwrap().0, 74);
        let (tag, cfg) = elements.next().unwrap().unwrap();
        assert_eq!(tag, 75);
        assert_eq!(cfg.len(), 44 * 4);
        assert_eq!(word(cfg, 1), Ok(PEERS));
        assert_eq!(word(cfg, 28), Ok(MSDUS));
        assert!(init(&service, &[]).is_err());
        assert!(init(&service, &[MemoryChunk { length: 4, ..chunk }]).is_err());
        let mut body = words(&ABI);
        body.extend_from_slice(&[2, 3, 4, 5, 6, 7, 0, 0]);
        body.extend_from_slice(&0u32.to_le_bytes());
        let mut packet = 2u32.to_le_bytes().to_vec();
        put_tlv(&mut packet, 35, &body).unwrap();
        assert_eq!(ready(&packet), Ok([2, 3, 4, 5, 6, 7]));
        for end in 0..packet.len() {
            assert!(ready(&packet[..end]).is_err());
        }
        body[32] = 1;
        let mut packet = 2u32.to_le_bytes().to_vec();
        put_tlv(&mut packet, 35, &body).unwrap();
        assert!(ready(&packet).is_err());
    }
    #[test]
    fn passive_scan_uses_factory_limits_and_never_enables_active_probes() {
        let service = service_ready(&event()).unwrap();
        let frequencies = passive_channels(&service);
        assert_eq!(frequencies.len(), 22);
        assert!(frequencies.contains(&2472));
        assert!(frequencies.contains(&5180));
        assert!(!frequencies.contains(&5260)); // DFS is outside discovery policy.
        assert_eq!(passive_frequencies([2412, 2462], [0, 0]).len(), 11);
        assert!(passive_frequencies([2473, 2484], [5260, 5720]).is_empty());
        let packet = start_scan(&frequencies).unwrap();
        let mut elements = Elements::new(&packet[4..]);
        let (tag, scan) = elements.next().unwrap().unwrap();
        assert_eq!(tag, 77);
        assert_eq!(scan.len(), 100);
        assert_eq!(word(scan, 14), Ok(0x21));
        assert_eq!(word(scan, 20), Ok(3));
        assert!(scan_channels(&[2484]).is_err());
        assert!(scan_channels(&[5260]).is_err());
        assert!(scan_channels(&[2413]).is_err());
        assert!(start_scan(&[]).is_err());
        assert!(start_scan_id(&frequencies, 0xa000).is_err());
        assert!(start_scan_id(&frequencies, 0xb000).is_err());
        assert!(vdev_create([0; 6]).is_err());
    }
    #[test]
    fn management_rx_checks_buffer_lengths_crc_and_channel_metadata() {
        let mut header = [0u32; 10];
        header[0] = 1;
        header[1] = 40;
        header[4] = 36;
        let mut packet = 0x7001u32.to_le_bytes().to_vec();
        put_tlv(&mut packet, 44, &words(&header)).unwrap();
        put_tlv(&mut packet, 17, &[0; 36]).unwrap();
        let event = management(&packet).unwrap();
        assert_eq!(event.frequency, 2412);
        assert_eq!(event.rssi, -55);
        assert_eq!(event.frame.len(), 36);
        for end in 0..packet.len() {
            assert!(management(&packet[..end]).is_err());
        }
        header[5] = 1;
        let mut bad = 0x7001u32.to_le_bytes().to_vec();
        put_tlv(&mut bad, 44, &words(&header)).unwrap();
        put_tlv(&mut bad, 17, &[0; 36]).unwrap();
        assert!(management(&bad).is_err());
    }
}
