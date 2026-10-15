//! PCI configuration mechanism 1. The lock covers the address/data port pair.
use crate::sync::IrqSpinlock;
use alloc::vec::Vec;
use osel_core::net::{memory_bar, pci_address};
use x86_64::instructions::port::Port;
static PCI_CONFIG: IrqSpinlock<()> = IrqSpinlock::new("PCI_CONFIG", ());
#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub bus: u8,
    pub slot: u8,
    pub function: u8,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub revision: u8,
}
impl Device {
    pub fn read(&self, offset: u8) -> u32 {
        read(self.bus, self.slot, self.function, offset)
    }
    pub fn enable_bus_master(&self) {
        let _guard = PCI_CONFIG.lock();
        unsafe {
            Port::<u32>::new(0xcf8).write(pci_address(self.bus, self.slot, self.function, 4));
            let command = Port::<u16>::new(0xcfc).read();
            // A word write must not acknowledge the adjacent write-one-to-clear status.
            Port::<u16>::new(0xcfc).write(command | 6);
        }
    }
    pub fn bar0(&self) -> Option<u64> {
        memory_bar(self.read(0x10), self.read(0x14))
    }
    /// Skip I/O BARs and the upper half of a 64-bit BAR.
    pub fn first_memory_bar(&self) -> Option<u64> {
        let mut index = 0;
        while index < 6 {
            let offset = 0x10 + index * 4;
            let low = self.read(offset);
            let high = if index < 5 { self.read(offset + 4) } else { 0 };
            if let Some(address) = memory_bar(low, high) {
                return Some(address);
            }
            index += if low & 7 == 4 { 2 } else { 1 };
        }
        None
    }
    pub fn read_word(&self, offset: u8) -> u16 {
        assert!(offset & 1 == 0);
        (self.read(offset) >> ((offset & 2) * 8)) as u16
    }
    pub fn write_word(&self, offset: u8, value: u16) {
        assert!(offset & 1 == 0);
        let _guard = PCI_CONFIG.lock();
        unsafe {
            Port::<u32>::new(0xcf8).write(pci_address(self.bus, self.slot, self.function, offset));
            Port::<u16>::new(0xcfc + (offset & 2) as u16).write(value);
        }
    }
    pub fn bus_master(&self, enabled: bool) {
        let command = self.read_word(4) | 2 | (1 << 10); // memory decoding, no INTx
        self.write_word(4, if enabled { command | 4 } else { command & !4 });
        self.read_word(4); // flush the configuration write
    }
    /// Polling drivers must also disable MSI/MSI-X left enabled by previous firmware.
    pub fn disable_interrupts(&self) {
        self.write_word(4, self.read_word(4) | (1 << 10));
        if let Some(msi) = self.capability(5) { self.write_word(msi + 2, self.read_word(msi + 2) & !1); }
        if let Some(msix) = self.capability(0x11) { self.write_word(msix + 2, (self.read_word(msix + 2) & !0x8000) | 0x4000); }
        self.read_word(4);
    }
    /// Capability chains are untrusted firmware data: bound traversal and validate pointers.
    fn capability(&self, id: u8) -> Option<u8> {
        if self.read_word(6) & (1 << 4) == 0 {
            return None;
        }
        let mut offset = self.read(0x34) as u8 & !3;
        let mut seen = 0u64;
        for _ in 0..48 {
            if offset < 0x40 {
                return None;
            }
            let bit = 1u64 << (offset / 4);
            if seen & bit != 0 {
                return None;
            }
            seen |= bit;
            let header = self.read(offset);
            if header as u8 == id {
                return Some(offset);
            }
            offset = (header >> 8) as u8 & !3;
        }
        None
    }
    /// Return whether a D3->D0 settling delay is needed. Disable PCIe ASPM/CLKREQ.
    pub fn wake(&self) -> bool {
        let mut settling = false;
        if let Some(pm) = self.capability(1).filter(|&p| p <= 0xf8) {
            let control = self.read_word(pm + 4);
            settling = control & 3 != 0;
            // PME status is write-one-to-clear: do not accidentally acknowledge it.
            self.write_word(pm + 4, control & !0x8003);
        }
        if let Some(pcie) = self.capability(0x10).filter(|&p| p <= 0xec) {
            let control = self.read_word(pcie + 0x10);
            self.write_word(pcie + 0x10, control & !0x0103);
        }
        settling
    }
}
fn read(bus: u8, slot: u8, function: u8, offset: u8) -> u32 {
    let _guard = PCI_CONFIG.lock();
    unsafe {
        Port::<u32>::new(0xcf8).write(pci_address(bus, slot, function, offset));
        Port::<u32>::new(0xcfc).read()
    }
}
pub fn scan() -> Vec<Device> {
    let mut devices = Vec::new();
    for bus in 0..=255 {
        for slot in 0..32 {
            if read(bus, slot, 0, 0) as u16 == 0xffff {
                continue;
            }
            let functions = if read(bus, slot, 0, 0x0c) & 0x00800000 != 0 {
                8
            } else {
                1
            };
            for function in 0..functions {
                let id = read(bus, slot, function, 0);
                if id as u16 == 0xffff {
                    continue;
                }
                let class = read(bus, slot, function, 8);
                devices.push(Device {
                    bus,
                    slot,
                    function,
                    vendor: id as u16,
                    device: (id >> 16) as u16,
                    class: (class >> 24) as u8,
                    subclass: (class >> 16) as u8,
                    revision: class as u8,
                });
            }
        }
    }
    devices
}
