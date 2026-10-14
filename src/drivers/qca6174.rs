// SPDX-License-Identifier: ISC
// Register definitions and reset/CE/BMI sequencing adapted from ath10k v6.12.
// Copyright (c) 2005-2011 Atheros Communications Inc.
// Copyright (c) 2011-2017 Qualcomm Atheros, Inc.
// Copyright (c) 2018 The Linux Foundation. All rights reserved.
// Copyright (c) 2022-2024 Qualcomm Innovation Center, Inc. All rights reserved.
// See firmware/ath10k-driver-ISC.txt for the license.
//! QCA6174 PCIe ROM bring-up and diagnostics, for the Acer LiteOn 11ad:0807 card.
//! Register/CE/BMI protocol definitions follow Linux ath10k (ISC), v6.12.
//! No firmware execution or RF transmission yet: this is a hardware checkpoint.
use super::{dma::Dma, pci};
use crate::{memory, sync::IrqSpinlock, time};
use core::{
    fmt::Write,
    sync::atomic::{AtomicBool, Ordering, fence},
};
use osel_core::{
    errno::{Errno, KResult},
    wifi::{self, Firmware},
};
use x86_64::{
    VirtAddr,
    structures::paging::{Mapper, Page, PageTableFlags as F, Size2MiB},
};

const LOCAL: usize = 0x80000;
const RTC: usize = 0x800;
const CORE: usize = 0x3a000;
const INDICATOR: usize = CORE + 0x28;
const CE: usize = 0x34400;
const ENTRIES: u32 = 16;
const LOG_CAPACITY: usize = 32;
static BUSY: AtomicBool = AtomicBool::new(false);
static REPORT: IrqSpinlock<Report> = IrqSpinlock::new("WIFI_REPORT", Report::new());

#[derive(Clone, Copy)]
struct Record {
    stage: &'static str,
    value: u32,
}
#[derive(Clone, Copy)]
struct Report {
    stage: &'static str,
    error: Option<Errno>,
    records: [Record; LOG_CAPACITY],
    len: usize,
    attempts: u32,
}
impl Report {
    const fn new() -> Self {
        Self {
            stage: "not probed",
            error: None,
            records: [Record {
                stage: "",
                value: 0,
            }; LOG_CAPACITY],
            len: 0,
            attempts: 0,
        }
    }
    fn record(&mut self, stage: &'static str, value: u32) {
        self.stage = stage;
        self.records[self.len.min(LOG_CAPACITY - 1)] = Record { stage, value };
        self.len = (self.len + 1).min(LOG_CAPACITY);
        crate::serial_println!("WIFI {}: {:08x}", stage, value);
        *REPORT.lock() = *self;
    }
}
struct Busy;
impl Busy {
    fn take() -> KResult<Self> {
        BUSY.compare_exchange(false, true, Ordering::Acquire, Ordering::Relaxed)
            .map_err(|_| Errno::EAGAIN)?;
        Ok(Self)
    }
}
impl Drop for Busy {
    fn drop(&mut self) {
        BUSY.store(false, Ordering::Release);
    }
}

fn card() -> Option<pci::Device> {
    pci::scan()
        .into_iter()
        .find(|d| (d.vendor, d.device) == (0x168c, 0x003e))
}
pub fn command(command: &str, out: &mut impl Write) -> KResult<()> {
    match command {
        "status" => {
            if let Some(d) = card() {
                let s = d.read(0x2c);
                let _ = writeln!(
                    out,
                    "WiFi QCA6174 {:02x}:{:02x}.{} 168c:003e rev={:02x}\n  subsystem={:04x}:{:04x} BAR0={:?}",
                    d.bus,
                    d.slot,
                    d.function,
                    d.revision,
                    s as u16,
                    s >> 16,
                    d.bar0()
                );
            } else {
                let _ = writeln!(out, "WiFi QCA6174 168c:003e: PCI card absent");
            }
            let report = *REPORT.lock();
            let _ = writeln!(
                out,
                "WiFi stage={} error={:?} attempts={} busy={}\nWiFi association/WPA2/data path are not implemented yet",
                report.stage,
                report.error,
                report.attempts,
                BUSY.load(Ordering::Acquire)
            );
        }
        "fw" => {
            let fw = Firmware::parse(wifi::FIRMWARE).map_err(|_| Errno::EIO)?;
            let board = wifi::board(wifi::BOARDS, wifi::ACER_BOARD).map_err(|_| Errno::EIO)?;
            let _ = writeln!(
                out,
                "WiFi firmware bundled, not running: {}\n  API=6 WMI={} HTT={} image={} OTP={} board={}\n  board match: LiteOn 11ad:0807, exact PCI identity\n  license: /usr/share/licenses/ath10k/LICENSE",
                fw.version,
                fw.wmi,
                fw.htt,
                fw.image.len(),
                fw.otp.len(),
                board.len()
            );
        }
        "regs" => {
            let report = *REPORT.lock();
            let _ = writeln!(out, "WiFi last probe (snapshot, no register reads):");
            for record in &report.records[..report.len] {
                let _ = writeln!(out, "  {} = {:08x}", record.stage, record.value);
            }
            if report.len == 0 {
                let _ = writeln!(out, "  No snapshot; run wifi probe on Acer");
            }
        }
        "probe" => {
            let _busy = Busy::take()?;
            let attempts = REPORT.lock().attempts.saturating_add(1);
            let mut report = Report::new();
            report.attempts = attempts;
            *REPORT.lock() = report;
            let _ = writeln!(
                out,
                "WiFi probe: PCI/D0/wake/reset/CE/BMI; bounded waits. See wifi regs."
            );
            let result = probe(&mut report);
            report.error = result.err();
            *REPORT.lock() = report;
            let _ = writeln!(
                out,
                "WiFi probe stage={} error={:?}",
                report.stage, report.error
            );
            result?;
        }
        _ => return Err(Errno::EINVAL),
    }
    Ok(())
}

struct Controller {
    device: pci::Device,
    base: usize,
    rings: Option<[Dma; 4]>, // CE0 source, CE1 destination, request, response
    tx: u32,
    rx: u32,
}
impl Controller {
    fn new(device: pci::Device) -> KResult<Self> {
        let physical = device.bar0().ok_or(Errno::ENXIO)?;
        if physical & 0xfffff != 0
            || physical
                .checked_add(0x100000)
                .is_none_or(|p| p > 0x1_0000_0000)
        {
            return Err(Errno::ENXIO);
        }
        let offset = memory::with_memory(|m| m.mapper.phys_offset());
        let base = (offset + physical).as_u64() as usize;
        // A 1 MiB BAR can straddle two direct-map huge pages.
        for address in [base, base + 0xfffff] {
            let page = Page::<Size2MiB>::containing_address(VirtAddr::new(address as u64));
            memory::with_memory(|m| unsafe {
                m.mapper
                    .update_flags(
                        page,
                        F::PRESENT | F::WRITABLE | F::NO_CACHE | F::WRITE_THROUGH | F::NO_EXECUTE,
                    )
                    .map(|f| f.flush())
            })
            .map_err(|_| Errno::ENXIO)?;
        }
        let mut controller = Self {
            device,
            base,
            rings: None,
            tx: 0,
            rx: 0,
        };
        controller.device.bus_master(false);
        controller.device.disable_interrupts();
        if controller.device.wake() {
            delay(20)?;
        }
        controller.put(LOCAL + 4, 1);
        controller.wait(LOCAL, 7, 3, 100)?;
        Ok(controller)
    }
    fn get(&self, offset: usize) -> u32 {
        unsafe { ((self.base + offset) as *const u32).read_volatile() }
    }
    fn put(&mut self, offset: usize, value: u32) {
        unsafe {
            ((self.base + offset) as *mut u32).write_volatile(value);
        }
        self.get(offset); // flush PCIe posted writes
    }
    fn wait(&self, offset: usize, mask: u32, expected: u32, ms: u64) -> KResult<()> {
        let end = time::monotonic_ns().saturating_add(ms * 1_000_000);
        loop {
            let value = self.get(offset);
            if value != u32::MAX && value & mask == expected {
                return Ok(());
            }
            if time::monotonic_ns() >= end {
                return Err(Errno::ETIMEDOUT);
            }
            delay(10)?;
        }
    }
    fn mask(&mut self) {
        self.put(CORE + 8, 0);
        self.put(CORE + 0x14, 0x7fc00);
        for i in 0..8 {
            self.put(CE + i * 0x400 + 0x2c, 0);
            self.put(CE + i * 0x400 + 0x34, 0);
        }
    }
    fn target(&mut self, report: &mut Report) -> KResult<()> {
        let end = time::monotonic_ns().saturating_add(3_000_000_000);
        loop {
            let value = self.get(INDICATOR);
            if value != u32::MAX {
                if value & 1 != 0 {
                    report.record("target crashed", value);
                    return Err(Errno::EIO);
                }
                if value & 2 != 0 {
                    report.record("target initialized", value);
                    self.mask();
                    return Ok(());
                }
                // ROM may race with the host while installing its PCIe core register mapping.
                // INTx remains disabled in PCI command; this write matches ath10k's handshake.
                self.put(CORE + 8, 0x7fc00);
            }
            if time::monotonic_ns() >= end {
                report.record("target timeout", value);
                self.mask();
                return Err(Errno::ETIMEDOUT);
            }
            delay(10)?;
        }
    }
    fn rings(&mut self) -> KResult<()> {
        let low = || {
            let dma = Dma::new().ok_or(Errno::ENOMEM)?;
            if dma.physical() >= 0x1_0000_0000 {
                return Err(Errno::ENXIO);
            }
            Ok(dma)
        };
        self.rings = Some([low()?, low()?, low()?, low()?]);
        Ok(())
    }
    fn pipes(&mut self) {
        self.mask();
        let rings = self.rings.as_ref().unwrap();
        unsafe {
            core::ptr::write_bytes(rings[0].ptr, 0, 4096);
            core::ptr::write_bytes(rings[1].ptr, 0, 4096);
        }
        let source = rings[0].physical() as u32;
        let dest = rings[1].physical() as u32;
        for i in 0..8 {
            let base = CE + i * 0x400;
            self.put(base + 4, 0);
            self.put(base + 0xc, 0);
            self.put(base + 0x18, 0); // clear halt for the ROM
            let control = self.get(base + 0x10) & !0x3ffff;
            self.put(base + 0x10, control | if i == 0 { 256 } else { 2048 });
        }
        self.put(CE, source);
        self.put(CE + 4, ENTRIES);
        self.put(CE + 0x400 + 8, dest);
        self.put(CE + 0x400 + 0xc, ENTRIES);
        self.put(CE + 0x4c, ENTRIES);
        self.put(CE + 0x400 + 0x50, ENTRIES);
        self.tx = self.get(CE + 0x3c) & (ENTRIES - 1);
        self.rx = self.get(CE + 0x400 + 0x40) & (ENTRIES - 1);
        fence(Ordering::SeqCst);
    }
    fn reset(&mut self, report: &mut Report) -> KResult<()> {
        self.device.bus_master(false);
        report.record("cold reset begin", 0);
        let global = self.get(LOCAL + 8);
        unsafe {
            ((self.base + LOCAL + 8) as *mut u32).write_volatile(global | 1);
        }
        delay(20)?; // PCIe accesses during reset can fault, including reads.
        unsafe {
            ((self.base + LOCAL + 8) as *mut u32).write_volatile(global & !1);
        }
        delay(20)?;
        self.put(LOCAL + 4, 1);
        self.wait(LOCAL, 7, 3, 100)?;
        report.record("cold reset RTC", self.get(LOCAL));
        self.target(report)?;
        self.rings()?;
        // ath10k QCA6174 cold+warm sequence. SI0 mask for QCA6174 is zero.
        self.mask();
        delay(20)?;
        self.put(INDICATOR, 0);
        self.put(RTC, self.get(RTC) | 0x40);
        self.pipes();
        self.target(report)?;
        self.put(RTC + 0x50, self.get(RTC + 0x50) & !4);
        let reset = self.get(RTC);
        self.put(RTC, reset | 1);
        delay(10)?;
        self.put(RTC, reset & !1);
        self.put(INDICATOR, 0);
        self.put(RTC, self.get(RTC) | 0x40);
        self.pipes();
        self.target(report)?;
        self.device.bus_master(true);
        report.record("CE0 write index", self.tx);
        report.record("CE1 write index", self.rx);
        Ok(())
    }
    fn bmi_info(&mut self, report: &mut Report) -> KResult<()> {
        report.record("BMI GET_TARGET_INFO begin", 8);
        let rings = self.rings.as_ref().unwrap();
        unsafe {
            rings[2].ptr.cast::<u32>().write_volatile(8u32.to_le());
            let tx = rings[0].ptr.add(self.tx as usize * 8);
            tx.cast::<u32>()
                .write_volatile((rings[2].physical() as u32).to_le());
            tx.add(4).cast::<u16>().write_volatile(4u16.to_le());
            tx.add(6).cast::<u16>().write_volatile(0);
            let rx = rings[1].ptr.add(self.rx as usize * 8);
            rx.cast::<u32>()
                .write_volatile((rings[3].physical() as u32).to_le());
            rx.add(4).cast::<u16>().write_volatile(0);
            rx.add(6).cast::<u16>().write_volatile(0);
        }
        let tx_next = (self.tx + 1) & (ENTRIES - 1);
        let rx_next = (self.rx + 1) & (ENTRIES - 1);
        fence(Ordering::SeqCst);
        self.put(CE + 0x400 + 0x40, rx_next);
        self.put(CE + 0x3c, tx_next);
        let sent = self.wait(CE + 0x44, ENTRIES - 1, tx_next, 3000);
        report.record("CE0 source read index", self.get(CE + 0x44));
        report.record("CE0 error status", self.get(CE + 0x38));
        sent?;
        let received = self.wait(CE + 0x400 + 0x48, ENTRIES - 1, rx_next, 3000);
        report.record("CE1 destination read index", self.get(CE + 0x400 + 0x48));
        report.record("CE1 error status", self.get(CE + 0x400 + 0x38));
        received?;
        // DRRI can be updated before the DMA descriptor write completes.
        // Wait for nbytes too, as ath10k does, rather than reporting a false short reply.
        let deadline = time::monotonic_ns().saturating_add(100_000_000);
        let length = loop {
            fence(Ordering::SeqCst);
            let rings = self.rings.as_ref().unwrap();
            let length = unsafe {
                u16::from_le(
                    rings[1]
                        .ptr
                        .add(self.rx as usize * 8 + 4)
                        .cast::<u16>()
                        .read_volatile(),
                )
            };
            if length != 0 || time::monotonic_ns() >= deadline {
                break length;
            }
            delay(10)?;
        };
        let rings = self.rings.as_ref().unwrap();
        report.record("BMI response length", u32::from(length));
        if length != 12 {
            return Err(Errno::EIO);
        }
        let read = |i| unsafe { u32::from_le(rings[3].ptr.add(i).cast::<u32>().read_volatile()) };
        let size = read(0);
        let version = read(4);
        let target = read(8);
        report.record("BMI info size", size);
        report.record("BMI target version", version);
        report.record("BMI target type", target);
        if size != 12 || version != 0x05030000 {
            return Err(Errno::EOPNOTSUPP);
        }
        Ok(())
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        // Disable bus mastering before descriptor pages are freed, including timeout paths.
        self.device.bus_master(false);
        self.mask();
        if self.rings.is_some() {
            for i in 0..8 {
                self.put(CE + i * 0x400 + 0x18, 1);
                self.put(CE + i * 0x400 + 4, 0);
                self.put(CE + i * 0x400 + 0xc, 0);
            }
        }
        self.put(LOCAL + 4, 0);
    }
}
fn delay(ms: u64) -> KResult<()> {
    time::sleep_until(time::monotonic_ns().saturating_add(ms * 1_000_000))
}
fn probe(report: &mut Report) -> KResult<()> {
    report.record("PCI scan begin", 0);
    let device = card().ok_or(Errno::ENODEV)?;
    report.record("PCI subsystem", device.read(0x2c));
    report.record("PCI revision", u32::from(device.revision));
    if device.read(0x2c) != 0x080711ad || device.revision != 0x32 {
        return Err(Errno::EOPNOTSUPP);
    }
    let _ = Firmware::parse(wifi::FIRMWARE).map_err(|_| Errno::EIO)?;
    let _ = wifi::board(wifi::BOARDS, wifi::ACER_BOARD).map_err(|_| Errno::EIO)?;
    report.record("PCI BAR0", device.read(0x10));
    report.record("D0/SOC_WAKE begin", 0);
    let mut controller = Controller::new(device)?;
    report.record("awake RTC", controller.get(LOCAL));
    report.record("SOC chip ID", controller.get(RTC + 0xf0));
    report.record("previous firmware indicator", controller.get(INDICATOR));
    controller.reset(report)?;
    controller.bmi_info(report)?;
    report.record("ROM ready; firmware not started", 0);
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test_case]
    fn missing_card_diagnostics_release_resources() {
        assert!(card().is_none(), "QEMU has no QCA6174 model");
        for _ in 0..2 {
            let mut out = osel_core::exception::FixedBuf::<2048>::new();
            command("fw", &mut out).unwrap();
            assert!(out.as_str().contains("board=8124"));
            assert_eq!(command("probe", &mut out), Err(Errno::ENODEV));
            assert!(!BUSY.load(Ordering::Acquire));
            assert_eq!(REPORT.lock().error, Some(Errno::ENODEV));
        }
    }
}
