// SPDX-License-Identifier: ISC
// Startup ABI adapted from Linux ath10k v6.12 (pci.c, ce.h, targaddrs.h).
// Copyright (c) 2005-2011 Atheros Communications Inc.
// Copyright (c) 2011-2017 Qualcomm Atheros, Inc.
// See firmware/ath10k-driver-ISC.txt.
//! ROM early configuration must precede BMI, including on a cold boot.
use crate::errno::{Errno, KResult};

pub const INTERCONNECT: u32 = 0x4008f8;
pub const EARLY_ALLOC: u32 = 0x400900;
pub const OPTION2: u32 = 0x4008cc;

pub trait Io {
    fn read32(&mut self, address: u32) -> KResult<u32>;
    fn write(&mut self, address: u32, words: &[u32]) -> KResult<()>;
    fn wake_cpu(&mut self) -> KResult<()>;
}

/// Target RAM addresses only: reject corrupt ROM pointers before starting DMA.
pub fn ram_range(address: u32, length: usize) -> KResult<()> {
    if address & 3 != 0
        || address < 0x400000
        || length == 0
        || length > 4096
        || address
            .checked_add(length as u32)
            .is_none_or(|end| end > 0x500000)
    {
        return Err(Errno::EIO);
    }
    Ok(())
}

/// QCA6174 target CPU -> CE address; bit 20 selects the target RAM window.
pub fn ce_address(core_control: u32, address: u32) -> KResult<u32> {
    ram_range(address, 4)?;
    if core_control == u32::MAX {
        return Err(Errno::EIO);
    }
    Ok(((core_control & 0x7ff) << 21) | 0x100000 | (address & 0xfffff))
}

// Target-side rings, not the host-owned ring sizes. QCA6174 overrides CE5 to OUT.
const PIPES: [[u32; 6]; 10] = [
    [0, 2, 32, 256, 0, 0],
    [1, 1, 32, 2048, 0, 0],
    [2, 1, 64, 2048, 0, 0],
    [3, 2, 32, 2048, 0, 0],
    [4, 2, 256, 256, 0, 0],
    [5, 2, 32, 2048, 0, 0],
    [6, 3, 32, 4096, 0, 0],
    [7, 3, 0, 0, 0, 0],
    [8, 1, 64, 2048, 8, 0],
    [9, 3, 32, 2048, 8, 0],
];
const SERVICES: [[u32; 3]; 17] = [
    [0x104, 2, 3],
    [0x104, 1, 2],
    [0x102, 2, 3],
    [0x102, 1, 2],
    [0x101, 2, 3],
    [0x101, 1, 2],
    [0x103, 2, 3],
    [0x103, 1, 2],
    [0x100, 2, 3],
    [0x100, 1, 2],
    [1, 2, 0],
    [1, 1, 1],
    [0xfe00, 2, 0],
    [0xfe00, 1, 1],
    [0x300, 2, 4],
    [0x300, 1, 1],
    [0, 0, 0],
];

pub fn configure(io: &mut impl Io) -> KResult<()> {
    let state = io.read32(INTERCONNECT)?;
    ram_range(state, 36)?;
    let pipes = io.read32(state)?;
    let services = io.read32(state + 4)?;
    ram_range(pipes, PIPES.len() * 24)?;
    ram_range(services, SERVICES.len() * 12)?;
    io.write(pipes, PIPES.as_flattened())?;
    io.write(services, SERVICES.as_flattened())?;
    let flags = io.read32(state + 32)?;
    io.write(state + 32, &[flags & !1])?; // no target-side PCIe L1
    let early = io.read32(EARLY_ALLOC)?;
    io.write(EARLY_ALLOC, &[early | 0x6d8a0009])?; // hw3.2: nine IRAM banks
    let options = io.read32(OPTION2)?;
    io.write(OPTION2, &[options | 0x10])?; // HI_OPTION_EARLY_CFG_DONE
    // Do not kick the CPU until the last DMA write is observable by the target.
    if io.read32(OPTION2)? & 0x10 == 0 {
        return Err(Errno::EIO);
    }
    io.wake_cpu()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    struct Rom {
        ram: BTreeMap<u32, u32>,
        awake: bool,
        fail_write: bool,
    }
    impl Rom {
        fn new() -> Self {
            Self {
                ram: BTreeMap::from([
                    (INTERCONNECT, 0x401000),
                    (0x401000, 0x402000),
                    (0x401004, 0x403000),
                    (0x401020, 0x81),
                    (EARLY_ALLOC, 0x100),
                    (OPTION2, 0x20),
                ]),
                awake: false,
                fail_write: false,
            }
        }
    }
    impl Io for Rom {
        fn read32(&mut self, address: u32) -> KResult<u32> {
            self.ram.get(&address).copied().ok_or(Errno::EIO)
        }
        fn write(&mut self, address: u32, words: &[u32]) -> KResult<()> {
            if self.fail_write {
                return Err(Errno::ETIMEDOUT);
            }
            ram_range(address, words.len() * 4)?;
            for (i, &word) in words.iter().enumerate() {
                self.ram.insert(address + i as u32 * 4, word);
            }
            Ok(())
        }
        fn wake_cpu(&mut self) -> KResult<()> {
            assert_eq!(self.ram[&OPTION2], 0x30);
            assert_eq!(self.ram[&EARLY_ALLOC], 0x6d8a0109);
            assert_eq!(self.ram[&0x401020], 0x80);
            // ROM needs a CE0 destination before it can consume host BMI sends.
            assert_eq!(self.ram[&0x402008], 32);
            assert_eq!(self.ram[&(0x402000 + 7 * 24 + 8)], 0); // CE7 host only
            assert_eq!(self.ram[&(0x403000 + 15 * 12 + 8)], 1); // HTT on CE1
            assert_eq!(self.ram[&(0x403000 + 16 * 12)], 0); // terminator
            self.awake = true;
            Ok(())
        }
    }
    #[test]
    fn rom_configuration_precedes_cpu_wake_and_preserves_flags() {
        let mut rom = Rom::new();
        configure(&mut rom).unwrap();
        assert!(rom.awake);
    }
    #[test]
    fn corrupt_pointers_and_dma_timeout_never_signal_configuration_done() {
        for pointer in [0, u32::MAX, 0x401001, 0x4ffff0, 0x80000] {
            let mut rom = Rom::new();
            rom.ram.insert(INTERCONNECT, pointer);
            assert_eq!(configure(&mut rom), Err(Errno::EIO));
            assert_eq!(rom.ram[&OPTION2], 0x20);
            assert!(!rom.awake);
        }
        let mut rom = Rom::new();
        rom.fail_write = true;
        assert_eq!(configure(&mut rom), Err(Errno::ETIMEDOUT));
        assert!(!rom.awake);
        assert_eq!(rom.ram[&OPTION2], 0x20);
    }
    #[test]
    fn target_dma_uses_pcie_region_without_treating_target_ram_as_host_ram() {
        assert_eq!(ce_address(0x2600, 0x4008f8), Ok(0xc01008f8));
        assert_eq!(ce_address(0x2600, 0x4ffffc), Ok(0xc01ffffc));
        assert!(ce_address(u32::MAX, 0x400800).is_err());
        for address in [0, 0x3ffffc, 0x400801, 0x500000, u32::MAX] {
            assert!(ce_address(0x2600, address).is_err());
        }
        assert!(ram_range(0x4ffffc, 8).is_err());
        assert!(ram_range(0x400800, usize::MAX).is_err());
    }
}
