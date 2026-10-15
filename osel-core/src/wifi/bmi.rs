// SPDX-License-Identifier: ISC
// BMI/host-interest/PLL ABI adapted from Linux ath10k v6.12.
// Copyright (c) 2005-2011 Atheros Communications Inc.
// Copyright (c) 2011-2017 Qualcomm Atheros, Inc.
// Copyright (c) 2018 The Linux Foundation. All rights reserved.
// Copyright (c) 2022-2024 Qualcomm Innovation Center, Inc. All rights reserved.
// See firmware/ath10k-driver-ISC.txt.
//! Bounded BMI command encoding and QCA6174 hw3.2 firmware loading.
use super::{Firmware, boot};
use crate::errno::{Errno, KResult};

pub trait Transport {
    /// An empty reply means no receive descriptor may be posted for this command.
    fn exchange(&mut self, request: &[u8], reply: &mut [u8]) -> KResult<usize>;
    fn pause(&mut self) -> KResult<()>;
    fn progress(&mut self, stage: &'static str, value: u32);
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{collections::BTreeMap, vec::Vec};
    struct Rom {
        memory: BTreeMap<u32, u8>,
        registers: BTreeMap<u32, u32>,
        requests: Vec<Vec<u8>>,
        stream: Vec<u8>,
        downloads: Vec<Vec<u8>>,
        otp_result: u32,
        short_reply: bool,
        fail_after: usize,
        pauses: usize,
        done: bool,
    }
    impl Rom {
        fn new() -> Self {
            let mut rom = Self {
                memory: BTreeMap::new(),
                registers: BTreeMap::new(),
                requests: Vec::new(),
                stream: Vec::new(),
                downloads: Vec::new(),
                otp_result: 0,
                short_reply: false,
                fail_after: usize::MAX,
                pauses: 0,
                done: false,
            };
            rom.put(0x400854, 0x450000);
            rom.put(0x4008ac, 0);
            rom.put(0x400810, 0x40000000);
            rom
        }
        fn put(&mut self, address: u32, value: u32) {
            for (i, byte) in value.to_le_bytes().iter().enumerate() {
                self.memory.insert(address + i as u32, *byte);
            }
        }
        fn get(&self, address: u32) -> u32 {
            u32::from_le_bytes(core::array::from_fn(|i| {
                *self.memory.get(&(address + i as u32)).unwrap_or(&0)
            }))
        }
    }
    impl Transport for Rom {
        fn exchange(&mut self, request: &[u8], reply: &mut [u8]) -> KResult<usize> {
            assert!(!self.done);
            assert!(request.len() <= 256);
            if self.requests.len() >= self.fail_after {
                return Err(Errno::ETIMEDOUT);
            }
            self.requests.push(request.to_vec());
            let word = |i| u32::from_le_bytes(request[i..i + 4].try_into().unwrap());
            match word(0) {
                1 => {
                    assert!(reply.is_empty());
                    self.done = true;
                }
                2 => {
                    assert_eq!(word(8), 4);
                    reply.copy_from_slice(&self.get(word(4)).to_le_bytes());
                }
                3 => {
                    assert!(reply.is_empty());
                    assert_eq!(request.len(), 12 + word(8) as usize);
                    assert_eq!(word(8) & 3, 0);
                    for (i, &byte) in request[12..].iter().enumerate() {
                        self.memory.insert(word(4) + i as u32, byte);
                    }
                }
                4 => {
                    assert_eq!(word(4), 0x1234);
                    assert_eq!(word(8), 0); // execute calibration, never program OTP/flash
                    assert_eq!(self.get(0x400858), 1);
                    assert_eq!(self.downloads.len(), 1);
                    reply.copy_from_slice(&self.otp_result.to_le_bytes());
                }
                6 => reply.copy_from_slice(
                    &self
                        .registers
                        .get(&word(4))
                        .copied()
                        .unwrap_or(0)
                        .to_le_bytes(),
                ),
                7 => {
                    assert!(reply.is_empty());
                    self.registers.insert(word(4), word(8));
                }
                13 => {
                    assert!(reply.is_empty());
                    match word(4) {
                        0x1234 => assert!(self.stream.is_empty()),
                        0 => self.downloads.push(core::mem::take(&mut self.stream)),
                        _ => panic!("bad stream address"),
                    }
                }
                14 => {
                    assert!(reply.is_empty());
                    assert_eq!(request.len(), word(4) as usize + 8);
                    assert_eq!(word(4) & 3, 0);
                    self.stream.extend_from_slice(&request[8..]);
                }
                _ => panic!("unexpected BMI command"),
            }
            Ok(if self.short_reply && !reply.is_empty() {
                reply.len() - 1
            } else {
                reply.len()
            })
        }
        fn pause(&mut self) -> KResult<()> {
            self.pauses += 1;
            Ok(())
        }
        fn progress(&mut self, _: &'static str, _: u32) {}
    }
    #[test]
    fn real_firmware_and_exact_board_reach_rom_in_order_without_changing_bytes() {
        let fw = Firmware::parse(super::super::FIRMWARE).unwrap();
        let board = super::super::board(super::super::BOARDS, super::super::ACER_BOARD).unwrap();
        assert!(fw.ignore_otp_result);
        let mut rom = Rom::new();
        rom.otp_result = 1; // firmware feature explicitly permits this ROM result
        let mut client = Client::new(&mut rom);
        client.upload(&fw, board).unwrap();
        client.done().unwrap();
        assert_eq!(client.done(), Err(Errno::EALREADY));
        assert_eq!(client.read32(0x400800), Err(Errno::EALREADY));
        assert_eq!(rom.get(0x400800), 2);
        assert_eq!(rom.get(0x400810), 0x40002208);
        assert_eq!(rom.get(0x404024), 176000000);
        assert_eq!(rom.get(0x400858), 1);
        for (i, &byte) in board.iter().enumerate() {
            assert_eq!(rom.memory[&(0x450000 + i as u32)], byte);
        }
        for (got, expected) in rom.downloads.iter().zip([fw.otp, fw.image]) {
            assert_eq!(&got[..expected.len()], expected);
            assert!(got[expected.len()..].iter().all(|&b| b == 0));
            assert_eq!(got.len(), (expected.len() + 3) & !3);
        }
        assert_eq!(rom.downloads.len(), 2);
        assert!(rom.done);
    }
    #[test]
    fn stream_boundaries_are_padded_once_and_flushed() {
        for length in [1, 3, 4, 247, 248, 249, 492, 496, 497] {
            let bytes: Vec<u8> = (0..length).map(|i| (i % 251) as u8).collect();
            let mut rom = Rom::new();
            Client::new(&mut rom).download(&bytes, "test").unwrap();
            let got = &rom.downloads[0];
            assert_eq!(&got[..length], bytes);
            assert_eq!(got.len(), (length + 3) & !3);
            assert!(got[length..].iter().all(|&b| b == 0));
        }
    }
    #[test]
    fn every_board_chunk_stays_within_bmi_and_target_ram_limits() {
        let bytes: Vec<u8> = (0..8124).map(|i| (i % 253) as u8).collect();
        let mut rom = Rom::new();
        Client::new(&mut rom).write(0x450000, &bytes).unwrap();
        for (i, byte) in bytes.into_iter().enumerate() {
            assert_eq!(rom.memory[&(0x450000 + i as u32)], byte);
        }
        let count = rom.requests.len();
        for address in [0, u32::MAX, 0x450001, 0x4ff000] {
            assert!(Client::new(&mut rom).write(address, &[0; 8124]).is_err());
        }
        assert_eq!(rom.requests.len(), count);
    }
    #[test]
    fn pll_busy_short_response_and_dma_failure_stop_startup() {
        let mut rom = Rom::new();
        rom.registers.insert(0x1244, 0x20);
        assert_eq!(Client::new(&mut rom).pll(), Err(Errno::ETIMEDOUT));
        assert_eq!(rom.pauses, 100);
        assert!(!rom.done && rom.downloads.is_empty());
        let mut rom = Rom::new();
        rom.short_reply = true;
        assert_eq!(Client::new(&mut rom).read32(0x400800), Err(Errno::EIO));
        let mut rom = Rom::new();
        rom.fail_after = 3;
        assert_eq!(
            Client::new(&mut rom).download(&[1; 1000], "test"),
            Err(Errno::ETIMEDOUT)
        );
        assert!(rom.downloads.is_empty() && !rom.done);
        let mut rom = Rom::new();
        rom.fail_after = 0;
        let mut client = Client::new(&mut rom);
        assert_eq!(client.done(), Err(Errno::ETIMEDOUT));
        assert_eq!(client.done(), Err(Errno::EALREADY));
    }
    #[test]
    fn all_reference_clocks_preserve_unrelated_register_bits() {
        for index in 0..8 {
            let mut rom = Rom::new();
            rom.registers.insert(0xb2c, index << 8);
            rom.registers.insert(0xaf4, 0x80000000);
            Client::new(&mut rom).pll().unwrap();
            assert_eq!(rom.registers[&0xaf4] & 0x80000000, 0x80000000);
            assert_eq!(rom.registers[&0x1014] & ((1 << 16) | (1 << 18)), 0);
            assert_eq!(rom.get(0x404020), 1);
        }
    }
}

pub struct Client<'a, T: Transport> {
    transport: &'a mut T,
    done: bool,
}
impl<'a, T: Transport> Client<'a, T> {
    pub fn new(transport: &'a mut T) -> Self {
        Self {
            transport,
            done: false,
        }
    }
    fn command(&mut self, id: u32, words: &[u32], data: &[u8], reply: &mut [u8]) -> KResult<()> {
        if self.done {
            return Err(Errno::EALREADY);
        }
        let header = 4 + words.len() * 4;
        if header + data.len() > 256 || reply.len() > 256 {
            return Err(Errno::EINVAL);
        }
        let mut request = [0u8; 256];
        request[..4].copy_from_slice(&id.to_le_bytes());
        for (i, word) in words.iter().enumerate() {
            request[4 + i * 4..8 + i * 4].copy_from_slice(&word.to_le_bytes());
        }
        request[header..header + data.len()].copy_from_slice(data);
        if id == 1 {
            self.done = true;
        } // even a failed send may have reached ROM
        let received = self
            .transport
            .exchange(&request[..header + data.len()], reply)?;
        if received != reply.len() {
            self.transport
                .progress("BMI response length invalid", received as u32);
            return Err(Errno::EIO);
        }
        Ok(())
    }
    pub fn done(&mut self) -> KResult<()> {
        self.command(1, &[], &[], &mut [])
    }
    fn read(&mut self, id: u32, words: &[u32]) -> KResult<u32> {
        let mut reply = [0; 4];
        self.command(id, words, &[], &mut reply)?;
        Ok(u32::from_le_bytes(reply))
    }
    pub fn read32(&mut self, address: u32) -> KResult<u32> {
        boot::ram_range(address, 4)?;
        self.read(2, &[address, 4])
    }
    pub fn write(&mut self, address: u32, data: &[u8]) -> KResult<()> {
        // Board data is larger than one page. Validate the entire range before the first write.
        if data.is_empty()
            || data.len() > 8192
            || address & 3 != 0
            || address < 0x400000
            || address
                .checked_add(data.len() as u32)
                .is_none_or(|end| end > 0x500000)
        {
            return Err(Errno::EINVAL);
        }
        for (i, chunk) in data.chunks(244).enumerate() {
            let mut padded = [0u8; 244];
            padded[..chunk.len()].copy_from_slice(chunk);
            let count = (chunk.len() + 3) & !3;
            if address
                .checked_add((i * 244 + count) as u32)
                .is_none_or(|end| end > 0x500000)
            {
                return Err(Errno::EINVAL);
            }
            self.command(
                3,
                &[address + (i * 244) as u32, count as u32],
                &padded[..count],
                &mut [],
            )?;
        }
        Ok(())
    }
    pub fn write32(&mut self, address: u32, value: u32) -> KResult<()> {
        self.write(address, &value.to_le_bytes())
    }
    fn reg_read(&mut self, address: u32) -> KResult<u32> {
        self.read(6, &[address])
    }
    fn reg_write(&mut self, address: u32, value: u32) -> KResult<()> {
        self.command(7, &[address, value], &[], &mut [])
    }
    fn reg_update(&mut self, address: u32, clear: u32, set: u32) -> KResult<()> {
        let value = self.reg_read(address)?;
        self.reg_write(address, (value & !clear) | set)
    }
    pub fn download(&mut self, data: &[u8], stage: &'static str) -> KResult<()> {
        if data.is_empty() || data.len() > 1024 * 1024 {
            return Err(Errno::EINVAL);
        }
        self.transport.progress(stage, 0);
        self.command(13, &[0x1234], &[], &mut [])?;
        let mut completed = 0;
        for chunk in data.chunks(248) {
            let mut padded = [0u8; 248];
            padded[..chunk.len()].copy_from_slice(chunk);
            let count = (chunk.len() + 3) & !3;
            self.command(14, &[count as u32], &padded[..count], &mut [])?;
            completed += chunk.len();
            if completed / 65536 != (completed - chunk.len()) / 65536 {
                self.transport.progress(stage, completed as u32);
            }
        }
        // A new zero-address stream flushes the target decompressor/cache.
        self.command(13, &[0], &[], &mut [])?;
        self.transport.progress(stage, data.len() as u32);
        Ok(())
    }
    fn pll_wait(&mut self) -> KResult<()> {
        for _ in 0..100 {
            if self.reg_read(0x1244)? & 0x20 == 0 {
                return Ok(());
            }
            self.transport.pause()?;
        }
        Err(Errno::ETIMEDOUT)
    }
    fn pll(&mut self) -> KResult<()> {
        // (divider, fractional divider, settle time); all eight EFUSE selections.
        const CLOCKS: [(u32, u32, u32); 8] = [
            (0xe, 0x2aaa8, 2400),
            (0x24, 0x2aaa8, 960),
            (0x1d, 0x15551, 1200),
            (0x1b, 0x4ec4, 1300),
            (0x12, 0x34b49, 1870),
            (0x12, 0x15551, 1920),
            (0x12, 0x26665, 2000),
            (0x1b, 0x4ec4, 2600),
        ];
        self.transport.progress("PLL configure begin", 0);
        let efuse = self.reg_read(0xb2c)?;
        if efuse == u32::MAX {
            return Err(Errno::EIO);
        }
        let index = (efuse >> 8) & 7;
        self.transport.progress("PLL reference clock index", index);
        let (div, frac, settle) = CLOCKS[index as usize];
        self.reg_update(0xaf4, 0x1fffff, frac | (1 << 18))?;
        self.reg_update(0x1018, 0x7ff, settle & 0x7ff)?;
        self.reg_update(0x914, 7, 1)?;
        self.write32(0x404028, 1)?;
        self.reg_update(0x1014, 0, div | (1 << 18))?;
        self.pll_wait()?;
        self.reg_update(0x1014, 1 << 16, 0)?;
        self.pll_wait()?;
        self.reg_update(0x820, 3, 1)?;
        self.reg_update(0x1014, 1 << 18, 0)?;
        self.write32(0x404020, 1)?;
        self.write32(0x404024, 176000000)?;
        self.transport.progress("PLL configured", 176000000);
        Ok(())
    }
    /// Leaves BMI active. The caller must post HTC RX before calling done().
    pub fn upload(&mut self, fw: &Firmware<'_>, board: &[u8]) -> KResult<()> {
        if fw.wmi != 4
            || fw.htt != 3
            || board.len() != 8124
            || !fw.otp.starts_with(b"SGMT")
            || !fw.image.starts_with(b"SGMT")
        {
            return Err(Errno::EOPNOTSUPP);
        }
        self.pll()?;
        self.transport.progress("firmware host interest begin", 0);
        self.write32(0x400800, 2)?; // HTC protocol version
        let options = self.read32(0x400810)?;
        self.write32(0x400810, options | 0x2208)?;
        self.write32(0x400844, 0)?; // little endian
        self.write32(0x400904, 0)?; // no firmware swap
        self.write32(0x4008bc, 0x003e)?;
        let ext = self.read32(0x4008ac)?;
        if ext != 0 {
            return Err(Errno::EOPNOTSUPP);
        } // QCA6174 has no extended board data
        let address = self.read32(0x400854)?;
        self.transport
            .progress("board data target address", address);
        self.write(address, board)?;
        self.write32(0x400858, 1)?;
        self.transport
            .progress("board data uploaded", board.len() as u32);
        self.download(fw.otp, "OTP upload bytes")?;
        self.transport.progress("OTP execute begin", 0x1234);
        let result = self.read(4, &[0x1234, 0])?;
        self.transport.progress("OTP execute result", result);
        if result != 0 && !fw.ignore_otp_result {
            return Err(Errno::EIO);
        }
        self.download(fw.image, "firmware upload bytes")?;
        self.write32(0x400814, 0)?; // disable firmware UART output
        Ok(())
    }
}
