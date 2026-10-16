//! Firmware-owned HTT buffers. Controller stops/reset DMA before these are dropped.
use super::{Dma, Errno, KResult, Report};
use alloc::{collections::VecDeque, vec::Vec};
use core::sync::atomic::{Ordering, fence};
use osel_core::wifi::htt;

#[derive(Default, Clone, Copy)]
pub(super) struct Diagnostics {
    pub wmi: u32,
    pub management: u32,
    pub rejected: u32,
    pub wrong_channel: u32,
    pub htt: u32,
    pub last_htt: u32,
}
impl Diagnostics {
    pub(super) fn record(self, report: &mut Report) {
        for (name, value) in [
            ("STA received WMI events", self.wmi),
            ("STA management frames", self.management),
            ("STA rejected management frames", self.rejected),
            ("STA wrong-channel frames", self.wrong_channel),
            ("STA received HTT events", self.htt),
            ("STA last HTT event ID", self.last_htt),
        ] {
            report.record(name, value);
        }
    }
}
pub(super) struct Data {
    ring: Dma,
    index: Dma,
    buffers: Vec<Dma>,
    allocation: usize,
    tx: Vec<Option<Dma>>,
    tx_management: [bool; htt::TX_LIMIT],
    completed: [Option<u8>; htt::TX_LIMIT],
    started_ns: [u64; htt::TX_LIMIT],
    pub(super) received: VecDeque<Vec<u8>>,
    pub(super) management: VecDeque<Vec<u8>>,
    pub(super) started: bool,
    pub(super) security: [u64; 2],
    pub(super) peer: htt::PeerIds,
    pub(super) station: [u8; 6],
    pub(super) ap: [u8; 6],
    pub(super) frequency: u16,
    pub(super) replay: htt::Replay,
    pub(super) authorized: bool,
    pub(super) drops: u64,
    pub(super) diagnostics: Diagnostics,
}
fn page() -> KResult<Dma> {
    let page = Dma::new().ok_or(Errno::ENOMEM)?;
    if page.physical() + 4096 > 0x1_0000_0000 {
        return Err(Errno::ENOMEM);
    }
    Ok(page)
}
impl Data {
    pub(super) fn new(station: [u8; 6]) -> KResult<Self> {
        let mut result = Self {
            ring: page()?,
            index: page()?,
            buffers: Vec::new(),
            allocation: 0,
            tx: (0..htt::TX_LIMIT).map(|_| None).collect(),
            tx_management: [false; htt::TX_LIMIT],
            completed: [None; htt::TX_LIMIT],
            started_ns: [0; htt::TX_LIMIT],
            received: VecDeque::new(),
            management: VecDeque::new(),
            started: false,
            security: [0; 2],
            peer: htt::PeerIds::default(),
            station,
            ap: [0; 6],
            frequency: 0,
            replay: htt::Replay::new(),
            authorized: false,
            drops: 0,
            diagnostics: Diagnostics::default(),
        };
        for _ in 0..htt::RX_POSTED {
            let page = page()?;
            result.publish(&page);
            result.buffers.push(page);
        }
        Ok(result)
    }
    fn publish(&mut self, page: &Dma) {
        unsafe {
            // MSDU_DONE is the last target write; clear it before publishing again.
            core::ptr::write_bytes(page.ptr, 0, htt::RX_DESCRIPTOR);
            self.ring
                .ptr
                .cast::<u32>()
                .add(self.allocation)
                .write_volatile((page.physical() as u32).to_le());
        }
        self.allocation = (self.allocation + 1) & (htt::RX_ENTRIES - 1);
        fence(Ordering::SeqCst);
        unsafe {
            self.index
                .ptr
                .cast::<u32>()
                .write_volatile((self.allocation as u32).to_le());
        }
    }
    pub(super) fn config(&self) -> KResult<Vec<u8>> {
        htt::rx_config(self.index.physical() as u32, self.ring.physical() as u32)
    }
    pub(super) fn handle(&mut self, event: &[u8], report: &mut Report) -> KResult<()> {
        self.diagnostics.htt = self.diagnostics.htt.saturating_add(1);
        self.diagnostics.last_htt = event.first().copied().map_or(u32::MAX, u32::from);
        self.handle_event(event, report)
            .inspect_err(|&error| report.rejected_event(true, event, error))
    }
    fn handle_event(&mut self, event: &[u8], report: &mut Report) -> KResult<()> {
        match htt::event(event)? {
            htt::Event::Peer { id, vdev, mac } if vdev == 0 && mac == self.ap => {
                self.peer.insert(id)?;
                report.record("HTT AP peer mapped", u32::from(id));
            }
            htt::Event::Peer { id, .. } if self.peer.contains(id) => {
                self.peer.remove(id);
                if self.peer.is_empty() {
                    self.authorized = false;
                    return Err(Errno::ENETUNREACH);
                }
            }
            htt::Event::Unmap(id) if self.peer.contains(id) => {
                self.peer.remove(id);
                report.record("HTT AP peer unmapped", u32::from(id));
                if self.peer.is_empty() {
                    self.authorized = false;
                    return Err(Errno::ENETUNREACH);
                }
            }
            htt::Event::Tx { status, ids } => {
                for id in ids.chunks_exact(2) {
                    let id = u16::from_le_bytes(id.try_into().unwrap());
                    if self.tx_management[id as usize] || status != 0 {
                        report.record(
                            "HTT TX completion id/status",
                            u32::from(id) << 8 | u32::from(status),
                        );
                    }
                    self.complete(id, status)?;
                }
            }
            htt::Event::ManagementTx { id, status } => {
                if !self
                    .tx_management
                    .get(id as usize)
                    .copied()
                    .unwrap_or(false)
                {
                    return Err(Errno::EIO);
                }
                report.record(
                    "HTT management completion id/status",
                    u32::from(id) << 8 | u32::from(status),
                );
                self.complete(id, status)?;
            }
            htt::Event::Rx {
                peer,
                vdev,
                info,
                descriptors,
            } => self.receive(descriptors, peer, vdev, info)?,
            htt::Event::Security { peer, flags } => {
                report.record(
                    "HTT security indication",
                    u32::from(peer) << 8 | u32::from(flags),
                );
                if self.peer.contains(peer) && flags & 0x7f == 6 {
                    self.security[(flags >> 7) as usize] += 1;
                }
            }
            htt::Event::Other(1 | 10 | 0x11) => return Err(Errno::EOPNOTSUPP),
            _ => (),
        }
        Ok(())
    }
    pub(super) fn transmit(
        &mut self,
        frame: &[u8],
        management: bool,
        encrypt: bool,
    ) -> KResult<(u16, Vec<u8>)> {
        let slot = self
            .tx
            .iter()
            .position(Option::is_none)
            .ok_or(Errno::EAGAIN)?;
        let page = page()?;
        let frame_address = page.physical() as u32 + 512;
        let command = htt::transmit(
            slot as u16,
            frame_address,
            page.physical() as u32,
            frame,
            management,
            encrypt,
        )?;
        unsafe {
            page.ptr.cast::<u32>().write_volatile(frame_address.to_le());
            page.ptr
                .add(4)
                .cast::<u32>()
                .write_volatile((frame.len() as u32).to_le());
            core::ptr::copy_nonoverlapping(frame.as_ptr(), page.ptr.add(512), frame.len());
        }
        self.completed[slot] = None;
        self.tx_management[slot] = management;
        self.started_ns[slot] = crate::time::monotonic_ns();
        self.tx[slot] = Some(page); // pinned until HTT TX completion, including send failure
        Ok((slot as u16, command))
    }
    pub(super) fn complete(&mut self, id: u16, status: u8) -> KResult<()> {
        let slot = id as usize;
        if slot >= self.tx.len() || self.tx[slot].is_none() {
            return Err(Errno::EIO);
        }
        self.tx[slot] = None;
        self.completed[slot] = Some(status);
        Ok(())
    }
    pub(super) fn completion(&mut self, id: u16) -> Option<u8> {
        self.completed.get_mut(id as usize)?.take()
    }
    pub(super) fn can_transmit(&self) -> bool {
        self.tx.iter().any(Option::is_none)
    }
    pub(super) fn expired_tx(&self, now: u64) -> Option<usize> {
        self.tx.iter().enumerate().position(|(i, page)| {
            page.is_some() && now.saturating_sub(self.started_ns[i]) >= 5_000_000_000
        })
    }
    pub(super) fn receive(
        &mut self,
        descriptors: &[u8],
        peer: u16,
        vdev: u8,
        info: u8,
    ) -> KResult<()> {
        if vdev != 0 || !self.peer.contains(peer) || info & 0x40 != 0 {
            return Err(Errno::EOPNOTSUPP);
        }
        for entry in descriptors.chunks_exact(8) {
            let address = u32::from_le_bytes(entry[..4].try_into().unwrap());
            let length = u16::from_le_bytes(entry[4..6].try_into().unwrap()) as usize;
            let slot = self
                .buffers
                .iter()
                .position(|page| page.physical() == u64::from(address))
                .ok_or(Errno::EIO)?;
            fence(Ordering::SeqCst);
            let page = &self.buffers[slot];
            let bytes = unsafe { core::slice::from_raw_parts(page.ptr, htt::RX_SIZE) };
            if info & 0x20 != 0 {
                if let Ok(frame) = htt::offload_eapol(bytes, peer, self.station, self.ap) {
                    if self.received.len() < 64 {
                        self.received.push_back(frame);
                    } else {
                        self.drops += 1;
                    }
                } else {
                    self.drops += 1;
                }
            } else {
                match htt::receive(bytes, length, self.station, self.ap) {
                    Ok(frame) => {
                        let permitted = (!frame.encrypted || self.replay.accept(&frame).is_ok())
                            && htt::ethernet_allowed(
                                &frame,
                                self.station,
                                self.ap,
                                self.authorized,
                            );
                        if permitted && self.received.len() < 64 {
                            self.received.push_back(frame.ethernet.to_vec());
                        } else {
                            self.drops += 1;
                        }
                    }
                    Err(_) => self.drops += 1,
                }
            }
            // Temporarily take the page to borrow the ring and buffer independently.
            let page = self.buffers.swap_remove(slot);
            self.publish(&page);
            self.buffers.push(page);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use osel_core::frames::FrameSource;
    #[test_case]
    fn acer_peer_aliases_share_keys_rx_replay_and_survive_one_unmap() {
        let mut data = Data::new([2, 3, 4, 5, 6, 8]).unwrap();
        data.ap = [2, 3, 4, 5, 6, 7];
        let mut report = Report::new();
        let mut map =
            *include_bytes!("../../../osel-core/tests/fixtures/ath10k-v6.12/peer-map.bin");
        let mut security =
            *include_bytes!("../../../osel-core/tests/fixtures/ath10k-v6.12/security.bin");
        for id in [15u16, 127] {
            map[2..4].copy_from_slice(&id.to_le_bytes());
            data.handle(&map, &mut report).unwrap();
            security[2..4].copy_from_slice(&id.to_le_bytes());
            data.handle(&security, &mut report).unwrap();
        }
        assert!(data.peer.contains(15) && data.peer.contains(127));
        assert_eq!(
            data.security,
            [0, 2],
            "SEC_IND may use either firmware alias"
        );
        data.authorized = true;
        map[2..4].copy_from_slice(&15u16.to_le_bytes());
        map[4] ^= 2; // firmware reassigns this ID to another MAC
        data.handle(&map, &mut report).unwrap();
        assert!(!data.peer.contains(15) && data.peer.contains(127) && data.authorized);
        map[4] ^= 2;
        data.handle(&map, &mut report).unwrap();
        let original =
            include_bytes!("../../../osel-core/tests/fixtures/ath10k-v6.12/encrypted-ethernet.bin");
        for (id, pn) in [(15u16, 7u32), (127, 8)] {
            let page = &data.buffers[0];
            unsafe {
                core::ptr::copy_nonoverlapping(original.as_ptr(), page.ptr, original.len());
                page.ptr.add(16).cast::<u32>().write_volatile(pn.to_le());
            }
            let mut rx = alloc::vec![0x12, 0];
            rx.extend_from_slice(&id.to_le_bytes());
            rx.extend_from_slice(&[0, 0, 1, 0]);
            rx.extend_from_slice(&(page.physical() as u32).to_le_bytes());
            rx.extend_from_slice(&[14, 0, 0, 0]);
            data.handle(&rx, &mut report).unwrap();
        }
        assert_eq!(data.received.len(), 2);
        data.handle(&[4, 0, 15, 0], &mut report).unwrap();
        assert!(data.authorized && data.peer.contains(127) && !data.peer.contains(15));
        security[2..4].copy_from_slice(&15u16.to_le_bytes());
        data.handle(&security, &mut report).unwrap();
        assert_eq!(
            data.security,
            [0, 2],
            "an unmapped alias cannot acknowledge a key"
        );
        assert_eq!(
            data.handle(&[4, 0, 127, 0], &mut report),
            Err(Errno::ENETUNREACH)
        );
        assert!(!data.authorized && data.peer.is_empty());
    }
    #[test_case]
    fn management_completion_cannot_release_data_or_a_stale_dma_slot() {
        let mut data = Data::new([2, 3, 4, 5, 6, 8]).unwrap();
        let mut report = Report::new();
        let completion = include_bytes!(
            "../../../osel-core/tests/fixtures/ath10k-v6.12/management-completed.bin"
        );
        let (id, _) = data.transmit(&[0; 30], true, false).unwrap();
        assert_eq!(id, 0);
        data.handle(completion, &mut report).unwrap();
        assert_eq!(data.completion(id), Some(0));
        assert!(data.tx[0].is_none());
        assert_eq!(data.handle(completion, &mut report), Err(Errno::EIO));
        let (id, _) = data.transmit(&[0; 60], false, false).unwrap();
        assert_eq!(id, 0);
        assert_eq!(data.handle(completion, &mut report), Err(Errno::EIO));
        assert!(
            data.tx[0].is_some(),
            "wrong completion type must preserve ownership"
        );
        data.handle(
            include_bytes!("../../../osel-core/tests/fixtures/ath10k-v6.12/tx-completed.bin"),
            &mut report,
        )
        .unwrap();
        assert_eq!(data.completion(id), Some(0));
    }
    #[test_case]
    fn legacy_and_extended_management_events_complete_pinned_auth_buffers() {
        let mut data = Data::new([2, 3, 4, 5, 6, 8]).unwrap();
        let mut report = Report::new();
        for completion in [
            include_bytes!(
                "../../../osel-core/tests/fixtures/ath10k-v6.12/management-completed-legacy.bin"
            )
            .as_slice(),
            include_bytes!(
                "../../../osel-core/tests/fixtures/ath10k-v6.12/management-completed.bin"
            )
            .as_slice(),
        ] {
            let (id, _) = data.transmit(&[0; 30], true, false).unwrap();
            let mut malformed = completion.to_vec();
            malformed[8..12].copy_from_slice(&3u32.to_le_bytes());
            assert_eq!(data.handle(&malformed, &mut report), Err(Errno::EIO));
            assert!(data.tx[id as usize].is_some());
            assert_eq!(data.completion(id), None);
            let entries = &report.records[..report.len];
            assert!(entries.iter().any(
                |r| r.stage == "HTT rejected event bytes" && r.value == completion.len() as u32
            ));
            assert_eq!(entries.last().unwrap().value, Errno::EIO as u32);
            data.handle(completion, &mut report).unwrap();
            assert!(data.tx[id as usize].is_none());
            assert_eq!(data.completion(id), Some(0));
        }
    }
    #[test_case]
    fn error_reports_keep_headers_without_security_material_or_rx_payload() {
        for htt in [true, false] {
            let mut report = Report::new();
            let mut event = [0xa5; 28];
            event[..4].copy_from_slice(&[0xb, 0x86, 7, 0]);
            report.rejected_event(htt, &event, Errno::EIO);
            assert_eq!(report.len, 3);
            assert_eq!(report.records[1].value, 0x0007860b);
            assert!(
                report.records[..report.len]
                    .iter()
                    .all(|r| r.value != 0xa5a5a5a5)
            );
            // Five compact hardware records, six counters and final wait errno
            // leave the rejecting header visible in wifi regs' last 22 lines.
            for _ in 0..5 {
                report.record("test hardware state", 0);
            }
            Diagnostics::default().record(&mut report);
            report.record("STA HTT TX completion wait", Errno::EIO as u32);
            assert!(report.len <= 22);
            assert_eq!(report.records[report.len - 1].value, 5);
        }
    }
    #[test_case]
    fn reference_events_preserve_peer_key_dma_and_receive_ownership() {
        const STA: [u8; 6] = [2, 3, 4, 5, 6, 8];
        const AP: [u8; 6] = [2, 3, 4, 5, 6, 7];
        let mut data = Data::new(STA).unwrap();
        data.ap = AP;
        let mut report = Report::new();
        data.handle(
            include_bytes!("../../../osel-core/tests/fixtures/ath10k-v6.12/peer-map.bin"),
            &mut report,
        )
        .unwrap();
        assert!(data.peer.contains(7));
        data.handle(
            include_bytes!("../../../osel-core/tests/fixtures/ath10k-v6.12/security.bin"),
            &mut report,
        )
        .unwrap();
        assert_eq!(
            data.security,
            [0, 1],
            "unicast acknowledgement is distinct from GTK"
        );
        data.handle(
            include_bytes!("../../../osel-core/tests/fixtures/ath10k-v6.12/group-security.bin"),
            &mut report,
        )
        .unwrap();
        assert_eq!(data.security, [1, 1]);
        let mut wrong_peer =
            *include_bytes!("../../../osel-core/tests/fixtures/ath10k-v6.12/security.bin");
        wrong_peer[2] = 8;
        data.handle(&wrong_peer, &mut report).unwrap();
        assert_eq!(data.security, [1, 1]);

        let (id, _) = data.transmit(&[0xaa; 60], false, true).unwrap();
        assert_eq!(id, 0);
        let completed =
            include_bytes!("../../../osel-core/tests/fixtures/ath10k-v6.12/tx-completed.bin");
        data.handle(completed, &mut report).unwrap();
        assert_eq!(data.completion(id), Some(0));
        assert_eq!(data.handle(completed, &mut report), Err(Errno::EIO));

        data.authorized = true;
        let original =
            include_bytes!("../../../osel-core/tests/fixtures/ath10k-v6.12/encrypted-ethernet.bin");
        for forged in [false, true] {
            let page = &data.buffers[0];
            unsafe {
                core::ptr::copy_nonoverlapping(original.as_ptr(), page.ptr, original.len());
            }
            if forged {
                unsafe {
                    // Valid encrypted traffic relayed from another Ethernet source
                    // must not enter the authenticator's EAPOL state machine.
                    page.ptr.add(16).write_volatile(8);
                    page.ptr.add(252).write_volatile(4);
                    page.ptr.add(306).write_volatile(4);
                    page.ptr.add(312).write_volatile(0x88);
                    page.ptr.add(313).write_volatile(0x8e);
                }
            }
            let mut event = alloc::vec![0x12, 0, 7, 0, 0, 0, 1, 0];
            event.extend_from_slice(&(page.physical() as u32).to_le_bytes());
            event.extend_from_slice(&[14, 0, 0, 0]);
            data.handle(&event, &mut report).unwrap();
            assert_eq!(data.buffers.len(), htt::RX_POSTED);
            assert_eq!(data.received.len(), 1);
        }
        assert_eq!(data.drops, 1);
        let mut bad_address = alloc::vec![0x12, 0, 7, 0, 0, 0, 1, 0];
        bad_address.extend_from_slice(&(data.index.physical() as u32).to_le_bytes());
        bad_address.extend_from_slice(&[14, 0, 0, 0]);
        assert_eq!(data.handle(&bad_address, &mut report), Err(Errno::EIO));
        assert_eq!(data.buffers.len(), htt::RX_POSTED);
        let unmap = include_bytes!("../../../osel-core/tests/fixtures/ath10k-v6.12/peer-unmap.bin");
        assert_eq!(data.handle(unmap, &mut report), Err(Errno::ENETUNREACH));
        assert!(data.peer.is_empty());
        assert!(!data.authorized);
    }
    #[test_case]
    fn htt_buffers_remain_pinned_until_firmware_completion() {
        let before = crate::memory::with_memory(|m| m.frame_allocator.used_frames());
        {
            let mut data = Data::new([2, 3, 4, 5, 6, 7]).unwrap();
            let allocated = crate::memory::with_memory(|m| m.frame_allocator.used_frames());
            assert_eq!(allocated, before + htt::RX_POSTED + 2);
            let (id, _) = data.transmit(&[0xaa; 60], false, false).unwrap();
            let started = data.started_ns[id as usize];
            assert_eq!(data.expired_tx(started + 4_999_999_999), None);
            assert_eq!(data.expired_tx(started + 5_000_000_000), Some(id as usize));
            assert_eq!(
                crate::memory::with_memory(|m| m.frame_allocator.used_frames()),
                allocated + 1
            );
            data.complete(id, 0).unwrap();
            assert_eq!(data.expired_tx(started + 5_000_000_000), None);
            assert_eq!(
                crate::memory::with_memory(|m| m.frame_allocator.used_frames()),
                allocated
            );
            assert_eq!(data.completion(id), Some(0));
            assert_eq!(data.completion(id), None);
            assert_eq!(data.complete(id, 0), Err(Errno::EIO));
        }
        assert_eq!(
            crate::memory::with_memory(|m| m.frame_allocator.used_frames()),
            before
        );
    }
}
