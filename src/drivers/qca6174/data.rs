//! Firmware-owned HTT buffers. Controller stops/reset DMA before these are dropped.
use super::{Dma, Errno, KResult};
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
                        let eapol = frame.ethernet[12..14] == [0x88, 0x8e]
                            && frame.ethernet[6..12] == self.ap
                            && frame.ethernet[..6] == self.station;
                        let permitted = (!frame.encrypted || self.replay.accept(&frame).is_ok())
                            && (eapol || (self.authorized && frame.encrypted));
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
    fn htt_buffers_remain_pinned_until_firmware_completion() {
        let before = crate::memory::with_memory(|m| m.frame_allocator.used_frames());
        {
            let mut data = Data::new([2, 3, 4, 5, 6, 7]).unwrap();
            let allocated = crate::memory::with_memory(|m| m.frame_allocator.used_frames());
            assert_eq!(allocated, before + htt::RX_POSTED + 2);
            let (id, _) = data.transmit(&[0xaa; 60], false, false).unwrap();
            assert_eq!(
                crate::memory::with_memory(|m| m.frame_allocator.used_frames()),
                allocated + 1
            );
            data.complete(id, 0).unwrap();
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
