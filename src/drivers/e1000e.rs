//! Intel 82574L (8086:10d3), legacy DMA descriptors. No interrupt or offload dependency.
//! The stack polls at 100 Hz. All waits are bounded; DMA memory survives while enabled.
use super::pci;
use crate::memory;
use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use x86_64::{
    VirtAddr,
    structures::paging::{FrameAllocator, FrameDeallocator, PhysFrame},
};
const COUNT: usize = 32;
const BUFFER: usize = 2048;
struct Dma {
    frame: PhysFrame,
    ptr: *mut u8,
}
unsafe impl Send for Dma {}
impl Dma {
    fn new() -> Option<Self> {
        let (frame, offset) =
            memory::with_memory(|m| (m.frame_allocator.allocate_frame(), m.mapper.phys_offset()));
        let frame = frame?;
        let ptr = (offset + frame.start_address().as_u64()).as_mut_ptr::<u8>();
        unsafe {
            core::ptr::write_bytes(ptr, 0, 4096);
        }
        Some(Self { frame, ptr })
    }
    fn physical(&self) -> u64 {
        self.frame.start_address().as_u64()
    }
}
impl Drop for Dma {
    fn drop(&mut self) {
        memory::with_memory(|m| unsafe { m.frame_allocator.deallocate_frame(self.frame) });
    }
}

pub struct E1000e {
    base: usize,
    rx_ring: Dma,
    tx_ring: Dma,
    rx: Vec<Dma>,
    tx: Vec<Dma>,
    rx_next: usize,
    tx_next: usize,
    pub mac: [u8; 6],
    pub received: u64,
    pub sent: u64,
}
unsafe impl Send for E1000e {}
impl E1000e {
    pub fn new(device: pci::Device) -> Option<Self> {
        if (device.vendor, device.device) != (0x8086, 0x10d3) {
            return None;
        }
        let physical = device.bar0()?;
        // Bootloader maps the low 4 GiB; do not silently dereference an unmapped 64-bit BAR.
        if physical >= 0x1_0000_0000 || physical & 0x1ffff != 0 {
            return None;
        }
        let offset = memory::with_memory(|m| m.mapper.phys_offset());
        let base = (offset + physical).as_u64() as usize;
        // The direct map uses huge pages. Make this MMIO page uncached before accessing it.
        use x86_64::structures::paging::{Mapper, Page, PageTableFlags as F, Size2MiB};
        let page = Page::<Size2MiB>::containing_address(VirtAddr::new(base as u64));
        memory::with_memory(|m| unsafe {
            m.mapper
                .update_flags(
                    page,
                    F::PRESENT | F::WRITABLE | F::NO_CACHE | F::WRITE_THROUGH | F::NO_EXECUTE,
                )
                .map(|f| f.flush())
        })
        .ok()?;
        let rx_ring = Dma::new()?;
        let tx_ring = Dma::new()?;
        let rx = (0..COUNT).map(|_| Dma::new()).collect::<Option<Vec<_>>>()?;
        let tx = (0..COUNT).map(|_| Dma::new()).collect::<Option<Vec<_>>>()?;
        device.enable_bus_master();
        let mut nic = Self {
            base,
            rx_ring,
            tx_ring,
            rx,
            tx,
            rx_next: 0,
            tx_next: 0,
            mac: [0; 6],
            received: 0,
            sent: 0,
        };
        nic.put(0xd8, u32::MAX); // IMC: polling, mask all device interrupts
        nic.put(0, nic.get(0) | (1 << 26)); // reset
        let mut reset = false;
        for _ in 0..1_000_000 {
            if nic.get(0) & (1 << 26) == 0 {
                reset = true;
                break;
            }
            core::hint::spin_loop();
        }
        if !reset {
            return None;
        }
        nic.put(0xd8, u32::MAX);
        let lo = nic.get(0x5400).to_le_bytes();
        let hi = nic.get(0x5404).to_le_bytes();
        nic.mac
            .copy_from_slice(&[lo[0], lo[1], lo[2], lo[3], hi[0], hi[1]]);
        if nic.mac == [0; 6] || nic.mac[0] & 1 != 0 {
            return None;
        }
        nic.put(0, nic.get(0) | (1 << 6) | (1 << 5)); // SLU + ASDE
        for i in 0..128 {
            nic.put(0x5200 + i * 4, 0);
        } // multicast filter
        for i in 0..COUNT {
            unsafe {
                nic.rx_ring
                    .ptr
                    .add(i * 16)
                    .cast::<u64>()
                    .write_volatile(nic.rx[i].physical());
                nic.tx_ring
                    .ptr
                    .add(i * 16)
                    .cast::<u64>()
                    .write_volatile(nic.tx[i].physical());
                nic.tx_ring.ptr.add(i * 16 + 12).write_volatile(1); // initially available
            }
        }
        for (ring, low, high, len, head, tail) in [
            (&nic.rx_ring, 0x2800, 0x2804, 0x2808, 0x2810, 0x2818),
            (&nic.tx_ring, 0x3800, 0x3804, 0x3808, 0x3810, 0x3818),
        ] {
            nic.put(low, ring.physical() as u32);
            nic.put(high, (ring.physical() >> 32) as u32);
            nic.put(len, (COUNT * 16) as u32);
            nic.put(head, 0);
            nic.put(tail, 0);
        }
        nic.put(0x2828, (1 << 24) | (1 << 16)); // RX descriptor write-back thresholds
        nic.put(0x3828, (1 << 24) | (1 << 16));
        nic.put(0x410, 0x0060200a); // TIPG
        fence(Ordering::SeqCst);
        nic.put(0x2818, (COUNT - 1) as u32);
        nic.put(0x100, (1 << 1) | (1 << 15) | (1 << 26)); // enable, broadcast, strip CRC, 2048-byte buffers
        nic.put(0x400, (1 << 1) | (1 << 3) | (15 << 4) | (64 << 12));
        Some(nic)
    }
    fn get(&self, reg: usize) -> u32 {
        unsafe { ((self.base + reg) as *const u32).read_volatile() }
    }
    fn put(&self, reg: usize, value: u32) {
        unsafe {
            ((self.base + reg) as *mut u32).write_volatile(value);
        }
    }
    pub fn link(&self) -> bool {
        self.get(8) & 2 != 0
    }
    fn tx_ready(&self) -> bool {
        unsafe { self.tx_ring.ptr.add(self.tx_next * 16 + 12).read_volatile() & 1 != 0 }
    }
    fn take_rx(&mut self) -> Option<Vec<u8>> {
        // Bounded even when every descriptor contains a malformed packet.
        for _ in 0..COUNT {
            let i = self.rx_next;
            let desc = unsafe { self.rx_ring.ptr.add(i * 16) };
            let status = unsafe { desc.add(12).read_volatile() };
            if status & 1 == 0 {
                return None;
            }
            fence(Ordering::Acquire);
            let len = unsafe { desc.add(8).cast::<u16>().read_volatile() } as usize;
            let errors = unsafe { desc.add(13).read_volatile() };
            let data = if status & 2 != 0 && errors == 0 && (14..=BUFFER).contains(&len) {
                Some(unsafe { core::slice::from_raw_parts(self.rx[i].ptr, len) }.to_vec())
            } else {
                None
            };
            unsafe {
                desc.add(12).write_volatile(0);
            }
            fence(Ordering::Release);
            self.put(0x2818, i as u32);
            self.rx_next = (i + 1) % COUNT;
            if data.is_some() {
                self.received += 1;
                return data;
            }
        }
        None
    }
}
impl Drop for E1000e {
    fn drop(&mut self) {
        self.put(0x100, 0);
        self.put(0x400, 0);
        self.put(0xd8, u32::MAX);
        self.get(8); // flush posted MMIO writes before returning DMA pages
    }
}
pub struct Receive(Vec<u8>);
impl RxToken for Receive {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}
pub struct Transmit<'a>(&'a mut E1000e);
impl TxToken for Transmit<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        assert!(len <= BUFFER);
        let nic = self.0;
        let i = nic.tx_next;
        let result = f(unsafe { core::slice::from_raw_parts_mut(nic.tx[i].ptr, len) });
        let desc = unsafe { nic.tx_ring.ptr.add(i * 16) };
        unsafe {
            desc.add(8).cast::<u16>().write_volatile(len as u16);
            desc.add(10).write_volatile(0);
            desc.add(11).write_volatile(1 | 2 | 8); // EOP, IFCS, RS
            desc.add(12).write_volatile(0);
            desc.add(13).write_volatile(0);
            desc.add(14).cast::<u16>().write_volatile(0);
        }
        fence(Ordering::Release);
        nic.tx_next = (i + 1) % COUNT;
        nic.put(0x3818, nic.tx_next as u32);
        nic.sent += 1;
        result
    }
}
impl Device for E1000e {
    type RxToken<'a> = Receive;
    type TxToken<'a> = Transmit<'a>;
    fn receive(&mut self, _: Instant) -> Option<(Receive, Transmit<'_>)> {
        if !self.tx_ready() {
            return None;
        }
        let packet = self.take_rx()?;
        Some((Receive(packet), Transmit(self)))
    }
    fn transmit(&mut self, _: Instant) -> Option<Transmit<'_>> {
        self.tx_ready().then_some(Transmit(self))
    }
    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.medium = Medium::Ethernet;
        caps.max_transmission_unit = 1514;
        caps.max_burst_size = Some(COUNT);
        caps
    }
}
