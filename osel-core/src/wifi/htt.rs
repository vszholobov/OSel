// SPDX-License-Identifier: ISC
// Protocol definitions follow Linux ath10k v6.12 (firmware/ath10k-driver-ISC.txt).
// Copyright (c) 2005-2011 Atheros Communications Inc.
// Copyright (c) 2011-2017 Qualcomm Atheros, Inc.
// Copyright (c) 2018 The Linux Foundation. All rights reserved.
// Copyright (c) 2022-2024 Qualcomm Innovation Center, Inc. All rights reserved.
//! Checked HTT TLV/32-bit PCI transport for QCA6174. No DMA access here.
use crate::errno::{Errno, KResult};
use alloc::{vec, vec::Vec};

pub const RX_SIZE: usize = 2048;
pub const RX_DESCRIPTOR: usize = 300;
pub const RX_ENTRIES: usize = 128;
pub const RX_POSTED: usize = RX_ENTRIES / 2 - 1;
pub const TX_LIMIT: usize = 64;
fn u16_at(data: &[u8], at: usize) -> KResult<u16> {
    Ok(u16::from_le_bytes(
        data.get(at..at + 2).ok_or(Errno::EIO)?.try_into().unwrap(),
    ))
}
fn u32_at(data: &[u8], at: usize) -> KResult<u32> {
    Ok(u32::from_le_bytes(
        data.get(at..at + 4).ok_or(Errno::EIO)?.try_into().unwrap(),
    ))
}
pub fn rx_config(index_address: u32, ring_address: u32) -> KResult<Vec<u8>> {
    if index_address == 0 || ring_address == 0 || index_address & 3 != 0 || ring_address & 3 != 0 {
        return Err(Errno::EINVAL);
    }
    let mut out = vec![2, 1, 0, 0];
    out.extend_from_slice(&index_address.to_le_bytes());
    out.extend_from_slice(&ring_address.to_le_bytes());
    for word in [
        RX_ENTRIES as u16,
        RX_SIZE as u16,
        0xffff,
        RX_POSTED as u16,
        59,
        75,
        21,
        31,
        3,
        20,
        6,
        10,
        1,
        2,
    ] {
        out.extend_from_slice(&word.to_le_bytes());
    }
    Ok(out)
}
pub enum Event<'a> {
    Version {
        major: u8,
        minor: u8,
    },
    Peer {
        id: u16,
        vdev: u8,
        mac: [u8; 6],
    },
    Unmap(u16),
    Tx {
        status: u8,
        ids: &'a [u8],
    },
    Rx {
        peer: u16,
        vdev: u8,
        info: u8,
        descriptors: &'a [u8],
    },
    Security {
        peer: u16,
        flags: u8,
    },
    Other(u8),
}
pub fn event(data: &[u8]) -> KResult<Event<'_>> {
    let id = *data.first().ok_or(Errno::EIO)?;
    Ok(match id {
        0 if data.len() >= 4 => Event::Version {
            major: data[2],
            minor: data[1],
        },
        3 if data.len() >= 12 => Event::Peer {
            id: u16_at(data, 2)?,
            vdev: data[1],
            mac: data[4..10].try_into().unwrap(),
        },
        4 if data.len() >= 4 => Event::Unmap(u16_at(data, 2)?),
        7 if data.len() >= 4 => {
            let count = data[2] as usize;
            if count == 0 || count > TX_LIMIT || data[1] & 7 > 3 {
                return Err(Errno::EIO);
            }
            let ids = data.get(4..4 + count * 2).ok_or(Errno::EIO)?;
            for (i, id) in ids.chunks_exact(2).enumerate() {
                if u16::from_le_bytes(id.try_into().unwrap()) as usize >= TX_LIMIT
                    || ids[..i * 2].chunks_exact(2).any(|old| old == id)
                {
                    return Err(Errno::EIO);
                }
            }
            Event::Tx {
                status: data[1] & 7,
                ids,
            }
        }
        0x12 if data.len() >= 8 => {
            let count = u16_at(data, 6)? as usize;
            if count == 0 || count > RX_POSTED {
                return Err(Errno::EIO);
            }
            let descriptors = data.get(8..8 + count * 8).ok_or(Errno::EIO)?;
            for (i, descriptor) in descriptors.chunks_exact(8).enumerate() {
                let address = u32_at(descriptor, 0)?;
                if address == 0
                    || address & 127 != 0
                    || u16_at(descriptor, 4)? as usize > RX_SIZE
                    || descriptors[..i * 8]
                        .chunks_exact(8)
                        .any(|old| old[..4] == descriptor[..4])
                {
                    return Err(Errno::EIO);
                }
            }
            Event::Rx {
                peer: u16_at(data, 2)?,
                vdev: data[4],
                info: data[1],
                descriptors,
            }
        }
        0xb if data.len() >= 28 => Event::Security {
            peer: u16_at(data, 2)?,
            flags: data[1],
        },
        0 | 3 | 4 | 7 | 0xb | 0x12 => return Err(Errno::EIO),
        other => Event::Other(other),
    })
}

/// CE completion consumes this command, while HTT completion owns the frame's lifetime.
/// `frame_address` and `fragments_address` must remain pinned until Event::Tx.
pub fn transmit(
    id: u16,
    frame_address: u32,
    fragments_address: u32,
    frame: &[u8],
    management: bool,
    encrypt: bool,
) -> KResult<Vec<u8>> {
    if id as usize >= TX_LIMIT
        || frame_address == 0
        || frame.len() < if management { 24 } else { 14 }
        || frame.len() > 1514
        || (!management && fragments_address == 0)
    {
        return Err(Errno::EINVAL);
    }
    let mut out = vec![
        1,
        if management { 0x61 } else { 0x40 } | if encrypt { 0 } else { 4 },
    ];
    let tid = if management { 17u16 } else { 16 };
    out.extend_from_slice(&((tid << 6) | (1 << 11)).to_le_bytes()); // vdev=0, postponed
    out.extend_from_slice(&(frame.len() as u16).to_le_bytes());
    out.extend_from_slice(&id.to_le_bytes());
    out.extend_from_slice(
        &(if management {
            frame_address
        } else {
            fragments_address
        })
        .to_le_bytes(),
    );
    out.extend_from_slice(&0xffffu32.to_le_bytes()); // classify peer on the home channel
    let length = frame.len().min(64);
    out.extend_from_slice(&frame[..length]);
    while out.len() % 4 != 0 {
        out.push(0);
    }
    Ok(out)
}
pub struct Received<'a> {
    pub ethernet: &'a [u8],
    pub encrypted: bool,
    pub multicast: bool,
    pub key_index: u8,
    pub tid: u8,
    pub pn: u64,
}
/// Firmware offload lacks the normal descriptor/PN. Only MIC-verified EAPOL
/// may use this path; application packets must pass the full RX checks.
pub fn offload_eapol(buffer: &[u8], peer: u16, station: [u8; 6], ap: [u8; 6]) -> KResult<Vec<u8>> {
    if buffer.len() < 7 || u16_at(buffer, 2)? != peer || buffer[4] != 0 {
        return Err(Errno::EIO);
    }
    let length = u16_at(buffer, 0)? as usize;
    let frame = buffer
        .get(7..7 + length)
        .filter(|frame| frame.len() >= 36)
        .ok_or(Errno::EIO)?;
    let fc = u16_at(frame, 0)?;
    if fc & 0x030c != 0x0208
        || fc & 0x0400 != 0
        || frame[4..10] != station
        || frame[10..16] != ap
        || frame[16..22] != ap
        || frame[24..32] != [0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e]
    {
        return Err(Errno::EACCES);
    }
    let mut ethernet = station.to_vec();
    ethernet.extend_from_slice(&ap);
    ethernet.extend_from_slice(&[0x88, 0x8e]);
    ethernet.extend_from_slice(&frame[32..]);
    Ok(ethernet)
}
/// Legacy STA receive path: no 802.11 fragments or A-MSDU are negotiated.
pub fn receive(
    buffer: &[u8],
    length: usize,
    station: [u8; 6],
    ap: [u8; 6],
) -> KResult<Received<'_>> {
    if buffer.len() < RX_DESCRIPTOR
        || length < 14
        || RX_DESCRIPTOR + length > buffer.len().min(RX_SIZE)
    {
        return Err(Errno::EIO);
    }
    let attention = u32_at(buffer, 4)?;
    let errors = (1 << 3)
        | (1 << 4)
        | (1 << 8)
        | (1 << 9)
        | (1 << 13)
        | (1 << 16)
        | (1 << 17)
        | (1 << 24)
        | (1 << 26)
        | (1 << 27)
        | (1 << 28)
        | (1 << 29)
        | (1 << 30);
    if attention & (1 << 31) == 0 || attention & errors != 0 || buffer[8] != 0 {
        return Err(Errno::EIO);
    }
    let mpdu = u32_at(buffer, 12)?;
    let format = u32_at(buffer, 32)? >> 8 & 3;
    let msdu_end = u32_at(buffer, 56)?;
    if format != 2 || msdu_end & 0xc000 != 0xc000 || msdu_end & (1 << 18) != 0 {
        return Err(Errno::EOPNOTSUPP);
    }
    let header = &buffer[236..300];
    let fc = u16_at(header, 0)?;
    if fc & 0x030c != 0x0208
        || fc & 0x400 != 0
        || header[10..16] != ap
        || (fc & 0x80 != 0 && header[24] & 0x80 != 0)
    {
        return Err(Errno::EACCES);
    }
    let ethernet = &buffer[RX_DESCRIPTOR..RX_DESCRIPTOR + length];
    if ethernet[..6] != header[4..10] || ethernet[6..12] != header[16..22] {
        return Err(Errno::EACCES);
    }
    if ethernet[..6] != station && ethernet[0] & 1 == 0 {
        return Err(Errno::EACCES);
    }
    let encrypted = mpdu & (1 << 13) != 0;
    if ethernet[0] & 1 == 0 && buffer[44] >> 6 != 0 {
        return Err(Errno::EACCES);
    }
    if encrypted && (mpdu >> 28 != 6 || fc & 0x4000 == 0) {
        return Err(Errno::EACCES);
    }
    if !encrypted && fc & 0x4000 != 0 {
        return Err(Errno::EACCES);
    }
    Ok(Received {
        ethernet,
        encrypted,
        multicast: ethernet[0] & 1 != 0,
        key_index: buffer[44] >> 6,
        tid: if attention & (1 << 6) != 0 {
            16
        } else {
            (u32_at(buffer, 20)? >> 28) as u8
        },
        pn: u32_at(buffer, 16)? as u64 | ((u32_at(buffer, 20)? & 0xffff) as u64) << 32,
    })
}
/// Per-key receive replay state. Replace a group row only after a genuinely new GTK.
pub struct Replay {
    pairwise: [u64; 17],
    groups: [[u64; 17]; 4],
}
impl Replay {
    pub const fn new() -> Self {
        Self {
            pairwise: [0; 17],
            groups: [[0; 17]; 4],
        }
    }
    pub fn reset_pairwise(&mut self) {
        self.pairwise.fill(0);
    }
    pub fn install_group(&mut self, index: u8, rsc: [u8; 8]) -> KResult<()> {
        if index > 3 || rsc[6..] != [0; 2] {
            return Err(Errno::EINVAL);
        }
        self.groups[index as usize].fill(u64::from_le_bytes(rsc));
        Ok(())
    }
    pub fn accept(&mut self, frame: &Received<'_>) -> KResult<()> {
        if !frame.encrypted || frame.tid > 16 || frame.key_index > 3 {
            return Err(Errno::EACCES);
        }
        let row = if frame.multicast {
            &mut self.groups[frame.key_index as usize]
        } else {
            &mut self.pairwise
        };
        let previous = &mut row[frame.tid as usize];
        if frame.pn <= *previous {
            return Err(Errno::EACCES);
        }
        *previous = frame.pn;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn qca6174_layout_matches_v612_c_offsets_and_preserves_all_rx_fields() {
        let command = rx_config(0x1000, 0x2000).unwrap();
        assert_eq!(command.len(), 40);
        assert_eq!(
            &command[20..],
            &[
                59, 0, 75, 0, 21, 0, 31, 0, 3, 0, 20, 0, 6, 0, 10, 0, 1, 0, 2, 0
            ]
        );
        let mut page = vec![0; RX_SIZE];
        page[4..8].copy_from_slice(&(0x80000040u32).to_le_bytes());
        page[12..16].copy_from_slice(&(0x60002000u32).to_le_bytes());
        page[16..20].copy_from_slice(&7u32.to_le_bytes());
        page[32..36].copy_from_slice(&0x200u32.to_le_bytes());
        page[56..60].copy_from_slice(&0xc000u32.to_le_bytes());
        page[236..238].copy_from_slice(&0x4208u16.to_le_bytes());
        let ap = [2, 3, 4, 5, 6, 7];
        let station = [2, 3, 4, 5, 6, 8];
        page[246..252].copy_from_slice(&ap);
        page[240..246].copy_from_slice(&station);
        page[300..306].copy_from_slice(&station);
        let frame = receive(&page, 14, station, ap).unwrap();
        assert!(frame.encrypted);
        assert_eq!(frame.pn, 7);
        assert_eq!(frame.tid, 16);
        let mut replay = Replay::new();
        replay.accept(&frame).unwrap();
        assert_eq!(replay.accept(&frame), Err(Errno::EACCES));
        page[4..8].copy_from_slice(&(0xa0000040u32).to_le_bytes());
        assert!(receive(&page, 14, station, ap).is_err());
    }
    #[test]
    fn malformed_completions_cannot_free_unknown_or_duplicate_dma() {
        assert!(matches!(
            event(&[7, 0, 2, 0, 1, 0, 2, 0]).unwrap(),
            Event::Tx { status: 0, .. }
        ));
        for bad in [
            &[7, 0, 2, 0, 1, 0, 1, 0][..],
            &[7, 0, 1, 0, 64, 0],
            &[7, 0, 2, 0, 1, 0],
        ] {
            assert!(event(bad).is_err());
        }
        let mut rx = vec![0x12, 0, 1, 0, 0, 0, 2, 0];
        rx.extend_from_slice(&[0, 0x10, 0, 0, 14, 0, 0, 0]);
        rx.extend_from_slice(&[0, 0x10, 0, 0, 14, 0, 0, 0]);
        assert!(event(&rx).is_err());
        rx[17] = 0x20;
        assert!(matches!(event(&rx).unwrap(), Event::Rx { .. }));
        for end in 0..8 {
            assert!(event(&rx[..end]).is_err());
        }
    }
    #[test]
    fn htt3_management_points_to_frame_and_ethernet_points_to_fragment_list() {
        let frame = [0xab; 25];
        let management = transmit(3, 0x1000, 0x2000, &frame, true, false).unwrap();
        assert_eq!(&management[..8], &[1, 0x65, 0x40, 0xc, 25, 0, 3, 0]);
        assert_eq!(u32_at(&management, 8).unwrap(), 0x1000);
        assert_eq!(&management[16..41], &frame);
        assert_eq!(management.len(), 44);
        let data = transmit(4, 0x1000, 0x2000, &frame, false, true).unwrap();
        assert_eq!(&data[..4], &[1, 0x40, 0, 0xc]);
        assert_eq!(u32_at(&data, 8).unwrap(), 0x2000);
    }
    #[test]
    fn descriptorless_offload_never_delivers_application_data() {
        let station = [2, 3, 4, 5, 6, 7];
        let ap = [2, 3, 4, 5, 6, 8];
        let mut buffer = vec![0; 7 + 36];
        buffer[..2].copy_from_slice(&36u16.to_le_bytes());
        buffer[2] = 9;
        buffer[7..9].copy_from_slice(&0x0208u16.to_le_bytes());
        buffer[11..17].copy_from_slice(&station);
        buffer[17..23].copy_from_slice(&ap);
        buffer[23..29].copy_from_slice(&ap);
        buffer[31..39].copy_from_slice(&[0xaa, 0xaa, 3, 0, 0, 0, 0x88, 0x8e]);
        let frame = offload_eapol(&buffer, 9, station, ap).unwrap();
        assert_eq!(&frame[12..14], &[0x88, 0x8e]);
        buffer[37..39].copy_from_slice(&[8, 0]);
        assert!(offload_eapol(&buffer, 9, station, ap).is_err());
        buffer[37..39].copy_from_slice(&[0x88, 0x8e]);
        assert!(offload_eapol(&buffer, 10, station, ap).is_err());
    }
}
