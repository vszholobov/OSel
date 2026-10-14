//! Polled Realtek G/GU/H/M Ethernet. C+ uses the same DMA rings for QEMU validation.
//! Register facts checked against FreeBSD re(4), Linux r8169 and QEMU sources.
//! Uses built-in PHY firmware; checksum/VLAN/segmentation offloads are disabled.
use super::{dma::Dma, pci};
use crate::{memory, sync::IrqSpinlock};
use alloc::vec::Vec;
use core::sync::atomic::{Ordering, fence};
use osel_core::rtl8168::{self as format, BUFFER_SIZE, Chip, OWN, RING_SIZE};
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use x86_64::{
    VirtAddr,
    instructions::port::Port,
    structures::paging::{Mapper, Page, PageTableFlags as F, Size2MiB},
};

// PIT channel 2 is independent of the scheduler's channel 0. It supplies real delays
// even while boot interrupts are disabled; never assume a CPU-dependent spin count.
static PIT2: IrqSpinlock<()> = IrqSpinlock::new("PIT2", ());
fn delay_us(us: u16) -> Result<(), &'static str> {
    let _guard = PIT2.lock();
    let count = ((us as u64 * 1_193_182).div_ceil(1_000_000)) as u16;
    unsafe {
        let mut gate = Port::<u8>::new(0x61);
        let old = gate.read();
        gate.write(old & !3); // gate low, speaker off
        Port::<u8>::new(0x43).write(0xb0); // channel 2, low/high, mode 0
        Port::<u8>::new(0x42).write(count as u8);
        Port::<u8>::new(0x42).write((count >> 8) as u8);
        gate.write((old & !3) | 1);
        let mut result = Err("PIT channel 2 did not finish");
        for _ in 0..1_000_000 {
            if gate.read() & 0x20 != 0 {
                result = Ok(());
                break;
            }
            core::hint::spin_loop();
        }
        let current = gate.read();
        gate.write(old & 3 | current & !3);
        result
    }
}

pub struct Rtl8168 {
    base: usize,
    pci: pci::Device,
    chip: Chip,
    rx_ring: Dma,
    tx_ring: Dma,
    rx: Vec<Dma>,
    tx: Vec<Dma>,
    rx_next: usize,
    tx_next: usize,
    pub mac: [u8; 6],
    pub received: u64,
    pub sent: u64,
    pub dropped: u64,
}
unsafe impl Send for Rtl8168 {}
impl Rtl8168 {
    pub fn matches(device: &pci::Device) -> bool {
        device.vendor == 0x10ec && matches!(device.device, 0x8168 | 0x8139)
    }
    pub fn new(device: pci::Device) -> Result<Self, &'static str> {
        if !Self::matches(&device) {
            return Err("unsupported PCI ID");
        }
        let physical = device.first_memory_bar().ok_or("no memory BAR")?;
        if physical >= 0xffff_ff00 || physical & 255 != 0 {
            return Err("unmapped or unaligned BAR");
        }
        let base = memory::with_memory(|m| (m.mapper.phys_offset() + physical).as_u64()) as usize;
        let page = Page::<Size2MiB>::containing_address(VirtAddr::new(base as u64));
        memory::with_memory(|m| unsafe {
            m.mapper
                .update_flags(
                    page,
                    F::PRESENT | F::WRITABLE | F::NO_CACHE | F::WRITE_THROUGH | F::NO_EXECUTE,
                )
                .map(|f| f.flush())
        })
        .map_err(|_| "MMIO mapping failed")?;
        device.bus_master(false);
        let settling = device.wake();
        if settling {
            delay_us(10_000)?;
        }
        // Firmware may have left the NIC running. Bus mastering stays off until all
        // descriptors have been installed, including every failed initialization path.
        let tx_config = unsafe { ((base + 0x40) as *const u32).read_volatile() };
        crate::serial_println!(
            "RTL8168: PCI rev {:02x}, TxConfig={:08x}, XID {:03x}",
            device.revision,
            tx_config,
            (tx_config >> 20) & 0x7cf
        );
        crate::println!(
            "NET: Realtek PCI rev {:02x}, XID {:03x}",
            device.revision,
            (tx_config >> 20) & 0x7cf
        );
        let chip = Chip::detect(device.vendor, device.device, tx_config)
            .ok_or("unknown MAC XID (not PCI revision)")?;
        let mut nic = Self {
            base,
            pci: device,
            chip,
            rx_ring: Dma::new().ok_or("RX ring allocation failed")?,
            tx_ring: Dma::new().ok_or("TX ring allocation failed")?,
            rx: (0..RING_SIZE)
                .map(|_| Dma::new())
                .collect::<Option<Vec<_>>>()
                .ok_or("RX buffers allocation failed")?,
            tx: (0..RING_SIZE)
                .map(|_| Dma::new())
                .collect::<Option<Vec<_>>>()
                .ok_or("TX buffers allocation failed")?,
            rx_next: 0,
            tx_next: 0,
            mac: [0; 6],
            received: 0,
            sent: 0,
            dropped: 0,
        };
        nic.write16(0x3c, 0); // no IRQ/MSI dependency
        nic.write8(0x37, 0x10); // MAC reset
        nic.wait8(0x37, 0x10, false, 1000, 100)?;
        nic.write16(0x3c, 0);
        for (i, byte) in nic.mac.iter_mut().enumerate() {
            *byte = unsafe { ((base + i) as *const u8).read_volatile() };
        }
        if nic.mac == [0; 6] || nic.mac[0] & 1 != 0 {
            return Err("invalid station MAC");
        }
        if chip.gigabit() {
            format::configure_phy(&nic)?;
        }
        nic.write8(0x50, 0xc0); // unlock protected configuration
        if chip.gigabit() {
            nic.write8(0x52, nic.read8(0x52) | 0x20); // driver loaded
            nic.write8(0x54, nic.read8(0x54) & !4); // no jumbo frames
            nic.write8(0x55, nic.read8(0x55) & !2);
        }
        // C+ must precede RX/TX setup; GBE does not use the C+ RX enable bit.
        nic.write16(0xe0, if chip.gigabit() { 0x0081 } else { 3 });
        for i in 0..RING_SIZE {
            unsafe {
                let rx = nic.rx_ring.ptr.add(i * 16);
                rx.add(8).cast::<u64>().write_volatile(nic.rx[i].physical());
                rx.cast::<u32>().write_volatile(format::receive_command(i));
                let tx = nic.tx_ring.ptr.add(i * 16);
                tx.add(8).cast::<u64>().write_volatile(nic.tx[i].physical());
                tx.cast::<u32>().write_volatile(format::ring_end(i));
            }
        }
        for (low, high, address) in [
            (0x20, 0x24, nic.tx_ring.physical()),
            (0xe4, 0xe8, nic.rx_ring.physical()),
        ] {
            nic.write32(high, (address >> 32) as u32);
            nic.write32(low, address as u32);
        }
        if chip.gigabit() {
            nic.write32(0xf0, nic.read32(0xf0) & !(1 << 19)); // RXDV gate open
            nic.write16(0xda, BUFFER_SIZE as u16);
            nic.write16(0xe2, 0); // no interrupt mitigation
        } else {
            nic.write8(0x37, 0x0c); // C+ configuration needs enabled MACs
        }
        nic.write32(0x40, 0x0300_0700); // IFG, 2048-byte TX DMA burst
        nic.write32(
            0x44,
            0x0000_e700 | 2 | 8 | if chip.gigabit() { 0x800 } else { 0 },
        );
        nic.write32(0x08, 0);
        nic.write32(0x0c, 0); // no multicast subscription
        nic.write8(0xec, 16);
        nic.write16(0x3e, 0xffff); // acknowledge pending events
        nic.write8(0x50, 0);
        fence(Ordering::SeqCst);
        device.bus_master(true);
        nic.write8(0x37, 0x0c);
        nic.read8(0x37); // flush posted writes
        Ok(nic)
    }
    pub fn name(&self) -> &'static str {
        self.chip.name()
    }
    pub fn link(&self) -> bool {
        if self.chip.gigabit() {
            self.read8(0x6c) & 2 != 0
        } else {
            self.read8(0x58) & 4 == 0
        }
    }
    fn read8(&self, reg: usize) -> u8 {
        unsafe { ((self.base + reg) as *const u8).read_volatile() }
    }
    fn read32(&self, reg: usize) -> u32 {
        unsafe { ((self.base + reg) as *const u32).read_volatile() }
    }
    fn write8(&self, reg: usize, value: u8) {
        unsafe {
            ((self.base + reg) as *mut u8).write_volatile(value);
        }
    }
    fn write16(&self, reg: usize, value: u16) {
        unsafe {
            ((self.base + reg) as *mut u16).write_volatile(value);
        }
    }
    fn write32(&self, reg: usize, value: u32) {
        unsafe {
            ((self.base + reg) as *mut u32).write_volatile(value);
        }
    }
    fn wait8(
        &self,
        reg: usize,
        mask: u8,
        set: bool,
        tries: usize,
        us: u16,
    ) -> Result<(), &'static str> {
        for _ in 0..tries {
            if (self.read8(reg) & mask != 0) == set {
                return Ok(());
            }
            delay_us(us)?;
        }
        Err("MAC reset timed out")
    }
    fn tx_ready(&self) -> bool {
        unsafe {
            self.tx_ring
                .ptr
                .add(self.tx_next * 16)
                .cast::<u32>()
                .read_volatile()
                & OWN
                == 0
        }
    }
    fn take_rx(&mut self) -> Option<Vec<u8>> {
        for _ in 0..RING_SIZE {
            let i = self.rx_next;
            let descriptor = unsafe { self.rx_ring.ptr.add(i * 16) };
            let status = unsafe { descriptor.cast::<u32>().read_volatile() };
            if status & OWN != 0 {
                return None;
            }
            fence(Ordering::Acquire);
            let packet = self
                .chip
                .received_len(status)
                .map(|len| unsafe { core::slice::from_raw_parts(self.rx[i].ptr, len) }.to_vec());
            unsafe {
                descriptor.add(4).cast::<u32>().write_volatile(0);
            }
            fence(Ordering::Release);
            unsafe {
                descriptor
                    .cast::<u32>()
                    .write_volatile(format::receive_command(i));
            }
            self.rx_next = (i + 1) % RING_SIZE;
            if packet.is_some() {
                self.received += 1;
                return packet;
            }
            self.dropped += 1;
        }
        None
    }
}
impl format::PhyIo for Rtl8168 {
    fn read8(&self, offset: usize) -> u8 {
        self.read8(offset)
    }
    fn write8(&self, offset: usize, value: u8) {
        self.write8(offset, value);
    }
    fn read32(&self, offset: usize) -> u32 {
        self.read32(offset)
    }
    fn write32(&self, offset: usize, value: u32) {
        self.write32(offset, value);
    }
    fn delay_us(&self, us: u16) -> Result<(), &'static str> {
        delay_us(us)
    }
}
impl Drop for Rtl8168 {
    fn drop(&mut self) {
        self.write16(0x3c, 0);
        self.write32(0x44, 0); // reject incoming frames
        if self.chip.gigabit() {
            self.write32(0xf0, self.read32(0xf0) | (1 << 19));
        }
        self.write8(0x37, 0);
        // Disabling bus mastering prevents stray DMA even if a reset never completes.
        self.pci.bus_master(false);
        self.write8(0x37, 0x10);
        let _ = self.wait8(0x37, 0x10, false, 1000, 100);
        self.read8(0x37);
        fence(Ordering::SeqCst);
    }
}
pub struct Receive(Vec<u8>);
impl RxToken for Receive {
    fn consume<R, F: FnOnce(&[u8]) -> R>(self, f: F) -> R {
        f(&self.0)
    }
}
pub struct Transmit<'a>(&'a mut Rtl8168);
impl TxToken for Transmit<'_> {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
        let nic = self.0;
        let i = nic.tx_next;
        // Pad short Ethernet frames with zeroes, never old packet contents.
        let wire_len = len.max(60);
        let command = format::transmit_command(i, wire_len).expect("Ethernet MTU");
        fence(Ordering::Acquire);
        let buffer = unsafe { core::slice::from_raw_parts_mut(nic.tx[i].ptr, wire_len) };
        buffer[len..].fill(0);
        let result = f(&mut buffer[..len]);
        let descriptor = unsafe { nic.tx_ring.ptr.add(i * 16) };
        unsafe {
            descriptor.add(4).cast::<u32>().write_volatile(0);
        }
        fence(Ordering::Release);
        unsafe {
            descriptor.cast::<u32>().write_volatile(command);
        }
        fence(Ordering::SeqCst);
        nic.write8(if nic.chip.gigabit() { 0x38 } else { 0xd9 }, 0x40);
        nic.tx_next = (i + 1) % RING_SIZE;
        nic.sent += 1;
        result
    }
}
impl Device for Rtl8168 {
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
        caps.max_burst_size = Some(RING_SIZE);
        caps
    }
}
