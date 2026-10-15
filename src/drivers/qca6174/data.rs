//! Firmware-owned HTT buffers. Controller stops/reset DMA before these are dropped.
use super::{Dma, Errno, KResult, Report};
use alloc::{collections::VecDeque, vec::Vec};
use core::sync::atomic::{Ordering, fence};
use osel_core::wifi::htt;

pub(super) struct Data {
    ring: Dma,
    index: Dma,
    buffers: Vec<Dma>,
    allocation: usize,
    tx: Vec<Option<Dma>>,
    completed: [Option<u8>; htt::TX_LIMIT],
    started_ns: [u64; htt::TX_LIMIT],
    pub(super) received: VecDeque<Vec<u8>>,
    pub(super) management: VecDeque<Vec<u8>>,
    pub(super) started: bool,
    pub(super) security: [u64; 2],
    pub(super) peer: Option<u16>,
    pub(super) station: [u8; 6],
    pub(super) ap: [u8; 6],
    pub(super) frequency: u16,
    pub(super) replay: htt::Replay,
    pub(super) authorized: bool,
    pub(super) drops: u64,
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
            completed: [None; htt::TX_LIMIT],
            started_ns: [0; htt::TX_LIMIT],
            received: VecDeque::new(),
            management: VecDeque::new(),
            started: false,
            security: [0; 2],
            peer: None,
            station,
            ap: [0; 6],
            frequency: 0,
            replay: htt::Replay::new(),
            authorized: false,
            drops: 0,
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
        match htt::event(event)? {
            htt::Event::Peer { id, vdev, mac } if vdev == 0 && mac == self.ap => {
                self.peer = Some(id);
                report.record("HTT AP peer mapped", u32::from(id));
            }
            htt::Event::Unmap(id) if self.peer == Some(id) => {
                self.authorized = false;
                self.peer = None;
                return Err(Errno::ENETUNREACH);
            }
            htt::Event::Tx { status, ids } => {
                for id in ids.chunks_exact(2) {
                    self.complete(u16::from_le_bytes(id.try_into().unwrap()), status)?;
                }
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
                if self.peer == Some(peer) && flags & 0x7f == 6 {
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
        if vdev != 0 || self.peer != Some(peer) || info & 0x40 != 0 {
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
        assert_eq!(data.peer, Some(7));
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
        assert_eq!(data.peer, None);
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
