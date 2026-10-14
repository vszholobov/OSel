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
                });
            }
        }
    }
    devices
}
