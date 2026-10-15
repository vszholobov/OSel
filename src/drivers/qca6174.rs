// SPDX-License-Identifier: ISC
// Register definitions and reset/CE/BMI sequencing adapted from ath10k v6.12.
// Copyright (c) 2005-2011 Atheros Communications Inc.
// Copyright (c) 2011-2017 Qualcomm Atheros, Inc.
// Copyright (c) 2018 The Linux Foundation. All rights reserved.
// Copyright (c) 2022-2024 Qualcomm Innovation Center, Inc. All rights reserved.
// See firmware/ath10k-driver-ISC.txt for the license.
//! QCA6174 PCIe ROM bring-up and diagnostics, for the Acer LiteOn 11ad:0807 card.
//! Register/CE/BMI protocol definitions follow Linux ath10k (ISC), v6.12.
//! Bounded firmware bring-up, WPA2/CCMP station and shared Ethernet diagnostics.
mod bridge;
mod data;
mod station;
use super::{
    dma::{Dma, DmaRegion},
    pci,
};
use crate::{memory, sync::IrqSpinlock, time};
use alloc::{collections::VecDeque, vec::Vec};
pub use bridge::{Receive as WifiReceive, Transmit as WifiTransmit, Wireless};
use core::{
    fmt::Write,
    sync::atomic::{AtomicBool, Ordering, fence},
};
use osel_core::{
    errno::{Errno, KResult},
    file::FileLike,
    wifi::{
        self, Firmware, bmi, boot, htc, htt,
        ieee80211::{self, AccessPoint},
        wmi,
    },
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
const DIAG: usize = CE + 7 * 0x400;
const LOG_CAPACITY: usize = 64;
static SCAN_RESULTS: IrqSpinlock<[Option<AccessPoint>; 20]> =
    IrqSpinlock::new("WIFI_SCAN", [None; 20]);
static BUSY: AtomicBool = AtomicBool::new(false);
static ACTIVE: AtomicBool = AtomicBool::new(false);
static STOP: AtomicBool = AtomicBool::new(false);
type Handoff = (Controller, Report, wifi::wpa2::Handshake, Wireless, Busy);
static HANDOFF: IrqSpinlock<Option<Handoff>> = IrqSpinlock::new("WIFI_HANDOFF", None);
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
        if self.len == LOG_CAPACITY {
            self.records.copy_within(1.., 0);
        }
        self.records[self.len.min(LOG_CAPACITY - 1)] = Record { stage, value };
        self.len = (self.len + 1).min(LOG_CAPACITY);
        crate::println!("WIFI {}: {:08x}", stage, value);
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
                "WiFi stage={} error={:?} attempts={} busy={} active={}",
                report.stage,
                report.error,
                report.attempts,
                BUSY.load(Ordering::Acquire),
                ACTIVE.load(Ordering::Acquire)
            );
            bridge::status(out);
        }
        "fw" => {
            let fw = Firmware::parse(wifi::FIRMWARE).map_err(|_| Errno::EIO)?;
            let board = wifi::board(wifi::BOARDS, wifi::ACER_BOARD).map_err(|_| Errno::EIO)?;
            let _ = writeln!(
                out,
                "WiFi bundled firmware: {}\n  API=6 WMI={} HTT={} image={} OTP={} board={}\n  board match: LiteOn 11ad:0807, exact PCI identity\n  license: /usr/share/licenses/ath10k/LICENSE",
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
            for record in &report.records[report.len.saturating_sub(22)..report.len] {
                let _ = writeln!(out, "  {} = {:08x}", record.stage, record.value);
            }
            if report.len == 0 {
                let _ = writeln!(out, "  No snapshot; run wifi probe on Acer");
            }
        }
        "results" => show_networks(out),
        "disconnect" => {
            STOP.store(true, Ordering::Release);
            let deadline = time::monotonic_ns().saturating_add(5_000_000_000);
            while ACTIVE.load(Ordering::Acquire) {
                if time::monotonic_ns() >= deadline {
                    return Err(Errno::ETIMEDOUT);
                }
                delay(10)?;
            }
            let _ = writeln!(out, "WiFi disconnected; see net for interface state");
        }
        "probe" | "load" | "scan" => {
            let _busy = Busy::take()?;
            let attempts = REPORT.lock().attempts.saturating_add(1);
            let mut report = Report::new();
            report.attempts = attempts;
            *REPORT.lock() = report;
            let _ = writeln!(
                out,
                "WiFi probe: PCI/D0/wake/reset/CE/BMI; bounded waits. See wifi regs."
            );
            if command != "probe" {
                let _ = writeln!(
                    out,
                    "WiFi firmware check: up to 120 seconds after ROM; Ctrl+C cancels. Target stops after the check."
                );
            }
            let result = probe(&mut report, command);
            report.error = result.err();
            *REPORT.lock() = report;
            let _ = writeln!(
                out,
                "WiFi probe stage={} error={:?}",
                report.stage, report.error
            );
            result?;
            if command == "scan" {
                show_networks(out);
            }
        }
        _ => return Err(Errno::EINVAL),
    }
    Ok(())
}

pub fn connect(index: usize, out: &mut impl Write) -> KResult<()> {
    let busy = Busy::take()?;
    crate::net::can_switch()?;
    let ap = SCAN_RESULTS
        .lock()
        .get(index.wrapping_sub(1))
        .copied()
        .flatten()
        .ok_or(Errno::EINVAL)?;
    wifi::station::supported(&ap)?;
    let device = card().ok_or(Errno::ENODEV)?;
    if device.read(0x2c) != 0x080711ad || device.revision != 0x32 {
        return Err(Errno::EOPNOTSUPP);
    }
    station::nonce(&mut [0; 32])?;
    let _ = writeln!(
        out,
        "WiFi network {index}; WPA2 password (input hidden, Ctrl+C cancels):"
    );
    let tty = crate::tty::Tty::new(osel_core::vfs::Stat::default());
    let mut reader = wifi::wpa2::PasswordReader::new();
    let password = loop {
        let mut byte = [0];
        if tty.read(0, &mut byte, false)? != 1 {
            continue;
        }
        if let Some(result) = reader.receive(byte[0]) {
            let _ = out.write_char('\n');
            break result?;
        }
    };
    let pmk = wifi::wpa2::derive(&password, ap.ssid())?;
    drop(password);
    let mut report = Report::new();
    report.attempts = REPORT.lock().attempts.saturating_add(1);
    *REPORT.lock() = report;
    let result = (|| {
        let mut controller = initialize(&mut report)?;
        controller.load(&mut report)?;
        let handshake = controller.associate(&ap, pmk, &mut report)?;
        controller.load_deadline = None; // connected operation has no human-input reader or load timeout
        Ok((controller, handshake))
    })();
    let (controller, handshake) = match result {
        Ok(value) => value,
        Err(error) => {
            report.error = Some(error);
            *REPORT.lock() = report;
            return Err(error);
        }
    };
    let link = Wireless::new(controller.mac);
    if let Err(error) = crate::net::use_wireless(link) {
        link.down();
        return Err(error);
    }
    STOP.store(false, Ordering::Release);
    ACTIVE.store(true, Ordering::Release);
    *HANDOFF.lock() = Some((controller, report, handshake, link, busy));
    if let Err(error) = crate::scheduler::spawn_kernel(connected_worker) {
        let handoff = HANDOFF.lock().take();
        link.down();
        drop(handoff);
        ACTIVE.store(false, Ordering::Release);
        crate::net::device_lost();
        return Err(error);
    }
    let _ = writeln!(out, "WiFi WPA2 link ready; requesting DHCP");
    if let Err(error) = crate::net::wait_ready() {
        let _ = writeln!(
            out,
            "DHCP not ready; check net ip, net route, wifi status. WiFi worker remains active."
        );
        return Err(error);
    }
    let _ = writeln!(out, "WiFi DHCP ready; use net ip and ping gateway");
    Ok(())
}
fn connected_worker() {
    let Some((mut controller, mut report, mut handshake, link, busy)) = HANDOFF.lock().take()
    else {
        ACTIVE.store(false, Ordering::Release);
        return;
    };
    let result = (|| {
        while !STOP.load(Ordering::Acquire) {
            controller.station_poll(&mut report)?;
            controller.eapol(&mut handshake, &mut report)?;
            link.dropped(core::mem::take(
                &mut controller.data.as_mut().unwrap().drops,
            ));
            for _ in 0..8 {
                let data = controller.data.as_mut().ok_or(Errno::ENODEV)?;
                if let Some(frame) = data.received.pop_front() {
                    link.received(frame);
                }
                if !data.can_transmit() {
                    break;
                }
                let Some(frame) = link.take_tx() else {
                    break;
                };
                controller.transmit_frame(&frame, false, true, &mut report)?;
            }
            // Drain RX independently of TX queue activity.
            for _ in 0..64 {
                let Some(frame) = controller.data.as_mut().unwrap().received.pop_front() else {
                    break;
                };
                link.received(frame);
            }
            delay(10)?;
        }
        Ok(())
    })();
    link.down();
    report.error = result.err();
    report.record("WiFi worker stopped", u32::from(report.error.is_some()));
    *REPORT.lock() = report;
    drop(controller); // BME off/reset before DMA, then release the busy guard
    crate::net::device_lost();
    drop(busy);
    ACTIVE.store(false, Ordering::Release);
}

fn show_networks(out: &mut impl Write) {
    let networks = *SCAN_RESULTS.lock();
    let _ = writeln!(out, "Last passive WiFi scan (2.4 GHz):");
    for (index, ap) in networks
        .iter()
        .enumerate()
        .filter_map(|(i, ap)| ap.as_ref().map(|ap| (i, ap)))
    {
        let security = if ap.wpa2() {
            "WPA2"
        } else if ap.capabilities & 0x10 == 0 {
            "OPEN"
        } else {
            "UNSUPPORTED"
        };
        let _ = write!(
            out,
            "{} {:02x}:{:02x}:{:02x}:{:02x}:{:02x}:{:02x} {}MHz {}dBm {} ",
            index + 1,
            ap.bssid[0],
            ap.bssid[1],
            ap.bssid[2],
            ap.bssid[3],
            ap.bssid[4],
            ap.bssid[5],
            ap.frequency,
            ap.rssi,
            security
        );
        if ap.hidden() {
            let _ = out.write_str("<hidden>");
        } else {
            for byte in ap.ssid() {
                for escaped in core::ascii::escape_default(*byte) {
                    let _ = out.write_char(escaped as char);
                }
            }
        }
        let _ = out.write_char('\n');
    }
    if networks.iter().all(Option::is_none) {
        let _ = writeln!(out, "No saved networks");
    }
}
struct Controller {
    device: pci::Device,
    base: usize,
    rings: Option<[Dma; 10]>, // CE0/1, BMI buffers, CE7, CE2 RX, CE3/4 TX, WMI buffer
    tx: u32,
    rx: u32,
    diag_tx: u32,
    diag_rx: u32,
    wmi_rx: u32,
    host_memory: Vec<DmaRegion>,
    data: Option<data::Data>,
    wmi_endpoint: u8,
    htt_endpoint: u8,
    wmi_credits: u16,
    wmi_credit_max: u16,
    wmi_credit_size: u16,
    wmi_sequence: u8,
    wmi_max_message: usize,
    wmi_events: VecDeque<Vec<u8>>,
    htt_events: VecDeque<Vec<u8>>,
    mac: [u8; 6],
    channel_limits: [u32; 2],
    load_deadline: Option<u64>,
    reset_on_drop: bool,
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
            diag_tx: 0,
            diag_rx: 0,
            wmi_rx: 0,
            host_memory: Vec::new(),
            data: None,
            wmi_endpoint: 0,
            htt_endpoint: 0,
            wmi_credits: 0,
            wmi_credit_max: 0,
            wmi_credit_size: 1,
            wmi_sequence: 0,
            wmi_max_message: 0,
            wmi_events: VecDeque::new(),
            htt_events: VecDeque::new(),
            mac: [0; 6],
            channel_limits: [0; 2],
            load_deadline: None,
            reset_on_drop: false,
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
        let end = self
            .load_deadline
            .unwrap_or(u64::MAX)
            .min(time::monotonic_ns().saturating_add(ms * 1_000_000));
        let mut spins = 0;
        loop {
            self.check_operation()?;
            let value = self.get(offset);
            if value != u32::MAX && value & mask == expected {
                return Ok(());
            }
            if time::monotonic_ns() >= end {
                return Err(Errno::ETIMEDOUT);
            }
            if spins < 64 {
                spins += 1;
                core::hint::spin_loop();
            } else {
                delay(10)?;
            }
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
        self.rings = Some([
            low()?,
            low()?,
            low()?,
            low()?,
            low()?,
            low()?,
            low()?,
            low()?,
            low()?,
            low()?,
        ]);
        Ok(())
    }
    fn pipes(&mut self) {
        self.mask();
        let rings = self.rings.as_ref().unwrap();
        for i in [0, 1, 4, 5, 6, 7, 8] {
            unsafe { core::ptr::write_bytes(rings[i].ptr, 0, 4096) };
        }
        let addresses = [0, 1, 4, 5, 6, 7, 8].map(|i| rings[i].physical() as u32);
        // Only initialize host-owned halves. The ROM owns CE0 destination and CE1
        // source; clearing those sizes after target initialization stalls BMI.
        for (base, source, address, size) in [
            (CE, true, addresses[0], ENTRIES),
            (CE + 0x400, false, addresses[1], ENTRIES),
            (DIAG, true, addresses[2], 2),
            (DIAG, false, addresses[3], 2),
            (CE + 2 * 0x400, false, addresses[4], ENTRIES),
            (CE + 3 * 0x400, true, addresses[5], ENTRIES),
            (CE + 4 * 0x400, true, addresses[6], ENTRIES),
        ] {
            let offset = if source { 0 } else { 8 };
            self.put(base + offset, address);
            self.put(base + offset + 4, size);
            let control = self.get(base + 0x10);
            let control = if source {
                (control & !0x1ffff)
                    | if base == CE || base == CE + 4 * 0x400 {
                        256
                    } else {
                        2048
                    }
            } else {
                control & !(1 << 17)
            };
            self.put(base + 0x10, control);
            self.put(base + if source { 0x4c } else { 0x50 }, size);
        }
        self.tx = self.get(CE + 0x3c) & (ENTRIES - 1);
        self.rx = self.get(CE + 0x400 + 0x40) & (ENTRIES - 1);
        self.diag_tx = self.get(DIAG + 0x3c) & 1;
        self.diag_rx = self.get(DIAG + 0x40) & 1;
        self.wmi_rx = self.get(CE + 2 * 0x400 + 0x40) & (ENTRIES - 1);
        fence(Ordering::SeqCst);
    }
    fn snapshot(&self, report: &mut Report, base: usize) {
        // Capture before Drop disables BME/halts rings. Never read registers in wifi regs.
        for (stage, value) in [
            ("PCI command/status", self.device.read(4)),
            ("PCIe core control", self.get(CORE)),
            ("firmware indicator", self.get(INDICATOR)),
            ("CE base", base as u32),
            ("CE source base", self.get(base)),
            ("CE source size", self.get(base + 4)),
            ("CE destination base", self.get(base + 8)),
            ("CE destination size", self.get(base + 0xc)),
            ("CE control 1", self.get(base + 0x10)),
            ("CE command", self.get(base + 0x18)),
            ("CE source write index", self.get(base + 0x3c)),
            ("CE source read index", self.get(base + 0x44)),
            ("CE destination write index", self.get(base + 0x40)),
            ("CE destination read index", self.get(base + 0x48)),
            ("CE error status", self.get(base + 0x38) & 0x7e0),
        ] {
            report.record(stage, value);
        }
    }
    fn diag(&mut self, address: u32, data: Option<&[u32]>, report: &mut Report) -> KResult<u32> {
        let length = data.map_or(4, |words| words.len() * 4);
        boot::ram_range(address, length)?;
        if length > 2048 {
            return Err(Errno::EINVAL);
        }
        report.record("CE7 target address", address);
        let target = boot::ce_address(self.get(CORE), address)?;
        let rings = self.rings.as_ref().unwrap();
        let host = rings[2].physical() as u32;
        if let Some(words) = data {
            for (i, word) in words.iter().enumerate() {
                unsafe {
                    rings[2]
                        .ptr
                        .add(i * 4)
                        .cast::<u32>()
                        .write_volatile(word.to_le())
                };
            }
        } else {
            unsafe { rings[2].ptr.cast::<u32>().write_volatile(0) };
        }
        let (source, dest) = if data.is_some() {
            (host, target)
        } else {
            (target, host)
        };
        unsafe {
            descriptor(rings[4].ptr, self.diag_tx, source, length as u16);
            descriptor(rings[5].ptr, self.diag_rx, dest, 0);
        }
        let tx_next = self.diag_tx ^ 1;
        let rx_next = self.diag_rx ^ 1;
        fence(Ordering::SeqCst);
        self.put(DIAG + 0x40, rx_next);
        self.put(DIAG + 0x3c, tx_next);
        let sent = self.wait(DIAG + 0x44, 1, tx_next, 1000);
        if let Err(error) = sent {
            self.snapshot(report, DIAG);
            report.record("CE7 source timeout", self.get(DIAG + 0x44));
            return Err(error);
        }
        let received = self.wait_descriptor(5, self.diag_rx, 1000);
        let count = match received {
            Ok(count) => count,
            Err(error) => {
                self.snapshot(report, DIAG);
                report.record("CE7 destination timeout", self.get(DIAG + 0x48));
                return Err(error);
            }
        };
        if count as usize != length {
            report.record("CE7 response length invalid", u32::from(count));
            return Err(Errno::EIO);
        }
        let rings = self.rings.as_ref().unwrap();
        let returned_address = unsafe {
            u32::from_le(
                rings[5]
                    .ptr
                    .add(self.diag_rx as usize * 8)
                    .cast::<u32>()
                    .read_volatile(),
            )
        };
        if returned_address != dest {
            report.record("CE7 destination address invalid", returned_address);
            return Err(Errno::EIO);
        }
        let value = unsafe { u32::from_le(rings[2].ptr.cast::<u32>().read_volatile()) };
        self.diag_tx = tx_next;
        self.diag_rx = rx_next;
        Ok(value)
    }
    fn wait_descriptor(&self, ring: usize, index: u32, ms: u64) -> KResult<u16> {
        let end = self
            .load_deadline
            .unwrap_or(u64::MAX)
            .min(time::monotonic_ns().saturating_add(ms * 1_000_000));
        let mut spins = 0;
        loop {
            fence(Ordering::SeqCst);
            let rings = self.rings.as_ref().unwrap();
            let length = unsafe {
                u16::from_le(
                    rings[ring]
                        .ptr
                        .add(index as usize * 8 + 4)
                        .cast::<u16>()
                        .read_volatile(),
                )
            };
            if length != 0 {
                return Ok(length);
            }
            if time::monotonic_ns() >= end {
                return Err(Errno::ETIMEDOUT);
            }
            if spins < 64 {
                spins += 1;
                core::hint::spin_loop();
            } else {
                delay(10)?;
            }
        }
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
        // Final host-ring initialization follows target init, without modifying ROM rings.
        self.pipes();
        self.device.bus_master(true);
        report.record("PCI DMA enabled", self.device.read(4));
        report.record("CE0 write index", self.tx);
        report.record("CE1 write index", self.rx);
        Ok(())
    }
    fn bmi_exchange(
        &mut self,
        request: &[u8],
        reply: &mut [u8],
        report: &mut Report,
    ) -> KResult<usize> {
        if request.is_empty() || request.len() > 256 || reply.len() > 256 {
            return Err(Errno::EINVAL);
        }
        if self
            .load_deadline
            .is_some_and(|end| time::monotonic_ns() >= end)
        {
            report.record("firmware operation deadline", 120);
            return Err(Errno::ETIMEDOUT);
        }
        let rings = self.rings.as_ref().unwrap();
        unsafe {
            core::ptr::copy_nonoverlapping(request.as_ptr(), rings[2].ptr, request.len());
            descriptor(
                rings[0].ptr,
                self.tx,
                rings[2].physical() as u32,
                request.len() as u16,
            );
        }
        let tx_next = (self.tx + 1) & (ENTRIES - 1);
        fence(Ordering::SeqCst);
        if !reply.is_empty() {
            self.post_receive();
        }
        self.put(CE + 0x3c, tx_next);
        if let Err(error) = self.wait(CE + 0x44, ENTRIES - 1, tx_next, 3000) {
            self.snapshot(report, CE);
            report.record("BMI CE0 source timeout", self.get(CE + 0x44));
            return Err(error);
        }
        self.tx = tx_next;
        if reply.is_empty() {
            return Ok(0);
        }
        let count = self.receive(3000, report)?;
        if count as usize > reply.len() {
            report.record("BMI response too long", u32::from(count));
            return Err(Errno::EIO);
        }
        let rings = self.rings.as_ref().unwrap();
        unsafe { core::ptr::copy_nonoverlapping(rings[3].ptr, reply.as_mut_ptr(), count as usize) };
        Ok(count as usize)
    }
    fn post_receive(&mut self) {
        let rings = self.rings.as_ref().unwrap();
        unsafe { descriptor(rings[1].ptr, self.rx, rings[3].physical() as u32, 0) };
        fence(Ordering::SeqCst);
        self.put(CE + 0x400 + 0x40, (self.rx + 1) & (ENTRIES - 1));
    }
    fn receive(&mut self, timeout: u64, report: &mut Report) -> KResult<u16> {
        let count = match self.wait_descriptor(1, self.rx, timeout) {
            Ok(count) if count <= 2048 => count,
            Ok(count) => {
                report.record("CE1 response too long", u32::from(count));
                return Err(Errno::EIO);
            }
            Err(error) => {
                self.snapshot(report, CE + 0x400);
                report.record("CE1 receive timeout", self.get(CE + 0x400 + 0x48));
                return Err(error);
            }
        };
        self.rx = (self.rx + 1) & (ENTRIES - 1);
        Ok(count)
    }
    fn bmi_info(&mut self, report: &mut Report) -> KResult<()> {
        report.record("BMI GET_TARGET_INFO begin", 8);
        let mut response = [0; 12];
        let length = self.bmi_exchange(&8u32.to_le_bytes(), &mut response, report)?;
        report.record("BMI response length", length as u32);
        if length != 12 {
            return Err(Errno::EIO);
        }
        let word = |i| u32::from_le_bytes(response[i..i + 4].try_into().unwrap());
        report.record("BMI info size", word(0));
        report.record("BMI target version", word(4));
        report.record("BMI target type", word(8));
        if word(0) != 12 || word(4) != 0x05030000 || word(8) != 8 {
            return Err(Errno::EOPNOTSUPP);
        }
        Ok(())
    }
    fn load(&mut self, report: &mut Report) -> KResult<()> {
        let fw = Firmware::parse(wifi::FIRMWARE).map_err(|_| Errno::EIO)?;
        let board = wifi::board(wifi::BOARDS, wifi::ACER_BOARD).map_err(|_| Errno::EIO)?;
        self.reset_on_drop = true;
        self.load_deadline = Some(time::monotonic_ns().saturating_add(120_000_000_000));
        bmi::Client::new(&mut Bmi {
            controller: self,
            report,
        })
        .upload(&fw, board)?;
        // The first firmware packet can arrive as soon as BMI_DONE is consumed.
        self.post_receive();
        report.record("BMI_DONE begin", 1);
        bmi::Client::new(&mut Bmi {
            controller: self,
            report,
        })
        .done()?;
        report.record("HTC ready wait", 0);
        let count = self.receive(5000, report)?;
        let mut packet = [0u8; 2048];
        let rings = self.rings.as_ref().unwrap();
        unsafe {
            core::ptr::copy_nonoverlapping(rings[3].ptr, packet.as_mut_ptr(), count as usize)
        };
        report.record("HTC packet bytes", u32::from(count));
        for (stage, offset) in [
            ("HTC header word 0", 0),
            ("HTC header word 1", 4),
            ("HTC message word 0", 8),
        ] {
            report.record(
                stage,
                u32::from_le_bytes(packet[offset..offset + 4].try_into().unwrap()),
            );
        }
        let ready = htc::ready(&packet[..count as usize])?;
        report.record("HTC transmit credits", u32::from(ready.credits));
        report.record("HTC credit size", u32::from(ready.credit_size));
        report.record("HTC endpoints", u32::from(ready.endpoints));
        report.record("HTC ready", 1);
        self.wmi_boot(&ready, report)?;
        Ok(())
    }
    fn htc_connect(
        &mut self,
        service: u16,
        credits: u16,
        sequence: u8,
        report: &mut Report,
    ) -> KResult<htc::Service> {
        report.record("HTC connect service", u32::from(service));
        let request = htc::connect(service, credits, sequence)?;
        let mut reply = [0u8; 256];
        let count = self.bmi_exchange(&request, &mut reply, report)?;
        report.record("HTC connect reply bytes", count as u32);
        let result = htc::connected(&reply[..count], service)?;
        report.record("HTC assigned endpoint", u32::from(result.endpoint));
        report.record("HTC maximum message", u32::from(result.max_message));
        Ok(result)
    }
    fn wmi_boot(&mut self, ready: &htc::Ready, report: &mut Report) -> KResult<()> {
        // CE2 can receive WMI as soon as control services are connected.
        let rings = self.rings.as_ref().unwrap();
        unsafe { descriptor(rings[6].ptr, self.wmi_rx, rings[9].physical() as u32, 0) };
        fence(Ordering::SeqCst);
        self.put(CE + 2 * 0x400 + 0x40, (self.wmi_rx + 1) & (ENTRIES - 1));
        let htt = self.htc_connect(0x300, 0, 0, report)?;
        let control = self.htc_connect(0x100, ready.credits, 1, report)?;
        if htt.endpoint == control.endpoint {
            return Err(Errno::EIO);
        }
        report.record("HTC setup complete begin", 5);
        self.bmi_exchange(&htc::setup(2), &mut [], report)?;
        self.wmi_endpoint = control.endpoint;
        self.htt_endpoint = htt.endpoint;
        self.wmi_credits = ready.credits;
        self.wmi_credit_max = ready.credits;
        self.wmi_credit_size = ready.credit_size;
        self.wmi_max_message = usize::from(control.max_message);
        self.post_receive(); // HTC credit-only reports and HTT events may also use CE1.
        report.record("WMI service ready wait", 0);
        let deadline = time::monotonic_ns().saturating_add(5_000_000_000);
        let service = wmi::wait_service_ready(|| {
            let left = deadline.saturating_sub(time::monotonic_ns()) / 1_000_000;
            if left == 0 {
                return Err(Errno::ETIMEDOUT);
            }
            let data = self.next_wmi(left, report)?;
            report.record("WMI packet bytes", data.len() as u32);
            for i in 0..(data.len() / 4).min(4) {
                report.record(
                    "WMI event header",
                    u32::from_le_bytes(data[i * 4..i * 4 + 4].try_into().unwrap()),
                );
            }
            Ok(data)
        })?;
        self.channel_limits = service.channels_2ghz;
        report.record("WMI firmware build", service.build);
        for value in service.abi {
            report.record("WMI ABI", value);
        }
        report.record("WMI RF chains", service.chains);
        report.record("WMI EEPROM domain", service.eeprom_domain);
        for value in service.channels_2ghz {
            report.record("WMI 2GHz channel limit", value);
        }
        report.record("WMI memory requests", service.requests.len() as u32);
        for request in &service.requests {
            report.record("WMI memory request ID", request.id);
            report.record("WMI memory unit size", request.unit_size);
            report.record("WMI memory flags", request.flags);
            report.record("WMI memory units", request.units);
        }
        report.record("WMI service ready", 1);
        self.wmi_initialize(&service, &control, ready, report)?;
        self.htt_boot(service.supports(65), report)?;
        Ok(())
    }
    fn send_htt(&mut self, command: &[u8], report: &mut Report) -> KResult<()> {
        self.check_operation()?;
        if self.htt_endpoint == 0 || command.is_empty() || command.len() + 8 > 256 {
            return Err(Errno::E2BIG);
        }
        let base = CE + 4 * 0x400;
        let index = self.get(base + 0x3c) & (ENTRIES - 1);
        let rings = self.rings.as_ref().unwrap();
        let mut header = [0; 8];
        header[0] = self.htt_endpoint;
        header[2..4].copy_from_slice(&(command.len() as u16).to_le_bytes());
        unsafe {
            core::ptr::copy_nonoverlapping(header.as_ptr(), rings[2].ptr, 8);
            core::ptr::copy_nonoverlapping(command.as_ptr(), rings[2].ptr.add(8), command.len());
            descriptor(
                rings[8].ptr,
                index,
                rings[2].physical() as u32,
                (command.len() + 8) as u16,
            );
        }
        fence(Ordering::SeqCst);
        let next = (index + 1) & (ENTRIES - 1);
        self.put(base + 0x3c, next);
        if let Err(error) = self.wait(base + 0x44, ENTRIES - 1, next, 3000) {
            self.snapshot(report, base);
            report.record("HTT CE4 transmit timeout", index);
            return Err(error);
        }
        Ok(())
    }
    fn next_htt(&mut self, timeout: u64, report: &mut Report) -> KResult<Vec<u8>> {
        if let Some(event) = self.htt_events.pop_front() {
            return Ok(event);
        }
        let deadline = time::monotonic_ns().saturating_add(timeout.saturating_mul(1_000_000));
        loop {
            let left = deadline.saturating_sub(time::monotonic_ns()) / 1_000_000;
            if left == 0 {
                return Err(Errno::ETIMEDOUT);
            }
            if let Some((endpoint, event)) = self.receive_event(left, report)? {
                if endpoint == self.htt_endpoint {
                    return Ok(event);
                }
                if self.wmi_events.len() >= 32 {
                    return Err(Errno::EAGAIN);
                }
                self.wmi_events.push_back(event);
            }
        }
    }
    fn htt_boot(&mut self, full_reorder: bool, report: &mut Report) -> KResult<()> {
        report.record("HTT full RX reorder service", u32::from(full_reorder));
        if !full_reorder {
            return Err(Errno::EOPNOTSUPP);
        }
        self.send_htt(&[0, 0, 0, 0], report)?;
        report.record("HTT version wait", 0);
        let deadline = time::monotonic_ns().saturating_add(3_000_000_000);
        let (major, minor) = htt::wait_version(|| {
            let left = deadline.saturating_sub(time::monotonic_ns()) / 1_000_000;
            if left == 0 {
                return Err(Errno::ETIMEDOUT);
            }
            let event = self.next_htt(left, report)?;
            report.record("HTT version packet bytes", event.len() as u32);
            for word in event.chunks(4).take(4) {
                let mut bytes = [0; 4];
                bytes[..word.len()].copy_from_slice(word);
                report.record("HTT version packet word", u32::from_le_bytes(bytes));
            }
            Ok(event)
        })?;
        report.record("HTT version major", u32::from(major));
        report.record("HTT version minor", u32::from(minor));
        if major != 3 {
            return Err(Errno::EOPNOTSUPP);
        }
        let data = data::Data::new(self.mac)?;
        let config = data.config()?;
        self.data = Some(data); // owns the ring before the firmware can see it
        self.send_htt(&config, report)?;
        report.record("HTT RX ring posted buffers", htt::RX_POSTED as u32);
        Ok(())
    }
    fn handle_htt(&mut self, event: &[u8], report: &mut Report) -> KResult<()> {
        let data = self.data.as_mut().ok_or(Errno::ENODEV)?;
        match htt::event(event)? {
            htt::Event::Peer { id, vdev, mac } if vdev == 0 && mac == data.ap => {
                data.peer = Some(id);
                report.record("HTT AP peer mapped", u32::from(id));
            }
            htt::Event::Unmap(id) if data.peer == Some(id) => {
                data.authorized = false;
                data.peer = None;
                return Err(Errno::ENETUNREACH);
            }
            htt::Event::Tx { status, ids } => {
                for id in ids.chunks_exact(2) {
                    data.complete(u16::from_le_bytes(id.try_into().unwrap()), status)?;
                }
            }
            htt::Event::Rx {
                peer,
                vdev,
                info,
                descriptors,
            } => data.receive(descriptors, peer, vdev, info)?,
            htt::Event::Security { peer, flags } => {
                report.record(
                    "HTT security indication",
                    u32::from(peer) << 8 | u32::from(flags),
                );
                if data.peer == Some(peer) && flags & 0x7f == 6 {
                    data.security[(flags >> 7) as usize] += 1;
                }
            }
            htt::Event::Other(1 | 10 | 0x11) => return Err(Errno::EOPNOTSUPP),
            _ => (),
        }
        Ok(())
    }
    fn transmit_frame(
        &mut self,
        frame: &[u8],
        management: bool,
        encrypt: bool,
        report: &mut Report,
    ) -> KResult<u16> {
        let (id, command) = self
            .data
            .as_mut()
            .ok_or(Errno::ENODEV)?
            .transmit(frame, management, encrypt)?;
        self.send_htt(&command, report)?;
        Ok(id)
    }
    fn post_wmi(&mut self) {
        let rings = self.rings.as_ref().unwrap();
        unsafe { descriptor(rings[6].ptr, self.wmi_rx, rings[9].physical() as u32, 0) };
        fence(Ordering::SeqCst);
        self.put(CE + 2 * 0x400 + 0x40, (self.wmi_rx + 1) & (ENTRIES - 1));
    }
    fn send_wmi(&mut self, endpoint: u8, command: &[u8], report: &mut Report) -> KResult<()> {
        self.check_operation()?;
        if command.len() + 8 > 2048 || command.len() > self.wmi_max_message {
            return Err(Errno::E2BIG);
        }
        let credits = (command.len() + 8).div_ceil(self.wmi_credit_size as usize) as u16;
        let deadline = time::monotonic_ns().saturating_add(3_000_000_000);
        while self.wmi_credits < credits {
            let left = deadline.saturating_sub(time::monotonic_ns()) / 1_000_000;
            if left == 0 {
                report.record("WMI credit timeout", u32::from(self.wmi_credits));
                return Err(Errno::ETIMEDOUT);
            }
            if let Some((endpoint, data)) = self.receive_event(left, report)? {
                if endpoint == self.wmi_endpoint {
                    if self.wmi_events.len() >= 32 {
                        return Err(Errno::EAGAIN);
                    }
                    self.wmi_events.push_back(data);
                } else {
                    if self.htt_events.len() >= 64 {
                        return Err(Errno::EAGAIN);
                    }
                    self.htt_events.push_back(data);
                }
            }
        }
        self.wmi_credits -= credits;
        let base = CE + 3 * 0x400;
        let index = self.get(base + 0x3c) & (ENTRIES - 1);
        let rings = self.rings.as_ref().unwrap();
        let mut header = [0u8; 8];
        header[0] = endpoint;
        header[1] = 1; // request credit updates
        header[5] = self.wmi_sequence;
        self.wmi_sequence = self.wmi_sequence.wrapping_add(1);
        header[2..4].copy_from_slice(&(command.len() as u16).to_le_bytes());
        unsafe {
            core::ptr::copy_nonoverlapping(header.as_ptr(), rings[2].ptr, 8);
            core::ptr::copy_nonoverlapping(command.as_ptr(), rings[2].ptr.add(8), command.len());
            descriptor(
                rings[7].ptr,
                index,
                rings[2].physical() as u32,
                (command.len() + 8) as u16,
            );
        }
        fence(Ordering::SeqCst);
        self.put(base + 0x3c, (index + 1) & (ENTRIES - 1));
        if let Err(error) = self.wait(base + 0x44, ENTRIES - 1, (index + 1) & (ENTRIES - 1), 3000) {
            self.snapshot(report, base);
            report.record("WMI transmit timeout", index);
            return Err(error);
        }
        // CE source completion means firmware has copied the command. In particular,
        // do not retain a PTK/GTK in the shared command page for the session lifetime.
        unsafe {
            core::ptr::write_bytes(self.rings.as_ref().unwrap()[2].ptr, 0, command.len() + 8);
        }
        Ok(())
    }
    fn wmi_initialize(
        &mut self,
        service: &wmi::ServiceReady,
        control: &htc::Service,
        ready: &htc::Ready,
        report: &mut Report,
    ) -> KResult<()> {
        let mut chunks = Vec::new();
        let mut total = 0usize;
        for request in &service.requests {
            let length = request.bytes()?;
            total = total.checked_add(length).ok_or(Errno::E2BIG)?;
            if total > 8 * 1024 * 1024 {
                return Err(Errno::E2BIG);
            }
            let region = DmaRegion::new(length).ok_or(Errno::ENOMEM)?;
            chunks.push(wmi::MemoryChunk {
                id: request.id,
                address: region.physical() as u32,
                length: region.length as u32,
            });
            self.host_memory.push(region);
        }
        let command = wmi::init(service, &chunks)?;
        let credits = (command.len() + 8).div_ceil(ready.credit_size as usize);
        if command.len() > control.max_message as usize || credits > ready.credits as usize {
            return Err(Errno::EOPNOTSUPP);
        }
        report.record("WMI INIT host memory bytes", total as u32);
        self.send_wmi(control.endpoint, &command, report)?;
        report.record("WMI ready wait", 0);
        let deadline = time::monotonic_ns().saturating_add(5_000_000_000);
        for _ in 0..32 {
            let left = deadline.saturating_sub(time::monotonic_ns()) / 1_000_000;
            if left == 0 {
                return Err(Errno::ETIMEDOUT);
            }
            let data = self.next_wmi(left, report)?;
            if data.len() < 4 {
                return Err(Errno::EIO);
            }
            let id = u32::from_le_bytes(data[..4].try_into().unwrap()) & 0xffffff;
            report.record("WMI event ID", id);
            if id == 2 {
                let mac = wmi::ready(&data)?;
                self.mac = mac;
                report.record(
                    "WMI MAC first four bytes",
                    u32::from_le_bytes(mac[..4].try_into().unwrap()),
                );
                report.record(
                    "WMI MAC last two bytes",
                    u32::from(u16::from_le_bytes(mac[4..].try_into().unwrap())),
                );
                report.record("WMI ready; checkpoint complete", 1);
                return Ok(());
            }
        }
        Err(Errno::ETIMEDOUT)
    }
    fn descriptor_length(&self, ring: usize, index: u32) -> u16 {
        fence(Ordering::SeqCst);
        let rings = self.rings.as_ref().unwrap();
        unsafe {
            u16::from_le(
                rings[ring]
                    .ptr
                    .add(index as usize * 8 + 4)
                    .cast::<u16>()
                    .read_volatile(),
            )
        }
    }
    fn check_operation(&self) -> KResult<()> {
        if ACTIVE.load(Ordering::Acquire) && STOP.load(Ordering::Acquire) {
            return Err(Errno::EINTR);
        }
        if self
            .load_deadline
            .is_some_and(|deadline| time::monotonic_ns() >= deadline)
        {
            return Err(Errno::ETIMEDOUT);
        }
        if self.load_deadline.is_some() {
            let terminal = crate::tty::Tty::new(osel_core::vfs::Stat::default());
            let mut byte = [0];
            if terminal.read(0, &mut byte, true) == Ok(1) && byte[0] == 3 {
                return Err(Errno::EINTR);
            }
        }
        Ok(())
    }
    fn receive_event(
        &mut self,
        timeout: u64,
        report: &mut Report,
    ) -> KResult<Option<(u8, Vec<u8>)>> {
        let deadline = self
            .load_deadline
            .unwrap_or(u64::MAX)
            .min(time::monotonic_ns().saturating_add(timeout.saturating_mul(1_000_000)));
        loop {
            self.check_operation()?;
            let (count, buffer, wmi_ring) = if self.descriptor_length(6, self.wmi_rx) != 0 {
                (self.descriptor_length(6, self.wmi_rx), 9, true)
            } else if self.descriptor_length(1, self.rx) != 0 {
                (self.descriptor_length(1, self.rx), 3, false)
            } else {
                if time::monotonic_ns() >= deadline {
                    self.snapshot(report, CE + 2 * 0x400);
                    report.record("firmware receive timeout", 0);
                    return Err(Errno::ETIMEDOUT);
                }
                delay(10)?;
                continue;
            };
            if count < 8 || count > 2048 {
                report.record("firmware RX descriptor bytes", u32::from(count));
                return Err(Errno::EIO);
            }
            let rings = self.rings.as_ref().unwrap();
            let packet =
                unsafe { core::slice::from_raw_parts(rings[buffer].ptr, count as usize) }.to_vec();
            if wmi_ring {
                self.wmi_rx = (self.wmi_rx + 1) & (ENTRIES - 1);
                self.post_wmi();
            } else {
                self.rx = (self.rx + 1) & (ENTRIES - 1);
                self.post_receive();
            }
            let returned = htc::credits(&packet, self.wmi_endpoint).map_err(|error| {
                report.record("HTC rejected RX bytes", u32::from(count));
                report.record("HTC rejected RX CE", if wmi_ring { 2 } else { 1 });
                for word in packet[..8].chunks_exact(4) {
                    report.record(
                        "HTC rejected RX header",
                        u32::from_le_bytes(word.try_into().unwrap()),
                    );
                }
                error
            })?;
            self.wmi_credits = self
                .wmi_credits
                .saturating_add(returned)
                .min(self.wmi_credit_max);
            let body = htc::payload(&packet)?;
            if body.is_empty() {
                return Ok(None);
            }
            if packet[0] != self.wmi_endpoint && packet[0] != self.htt_endpoint {
                report.record("HTC unexpected RX endpoint", u32::from(packet[0]));
                return Err(Errno::EIO);
            }
            return Ok(Some((packet[0], body.to_vec())));
        }
    }
    fn next_wmi(&mut self, timeout: u64, report: &mut Report) -> KResult<Vec<u8>> {
        if let Some(event) = self.wmi_events.pop_front() {
            return Ok(event);
        }
        let deadline = time::monotonic_ns().saturating_add(timeout.saturating_mul(1_000_000));
        loop {
            let left = deadline.saturating_sub(time::monotonic_ns()) / 1_000_000;
            if left == 0 {
                return Err(Errno::ETIMEDOUT);
            }
            if let Some((endpoint, event)) = self.receive_event(left, report)? {
                if endpoint == self.wmi_endpoint {
                    return Ok(event);
                }
                if self.htt_events.len() >= 64 {
                    return Err(Errno::EAGAIN);
                }
                self.htt_events.push_back(event);
            }
        }
    }
    fn scan(&mut self, report: &mut Report) -> KResult<[Option<AccessPoint>; 20]> {
        let frequencies: Vec<u32> = (1..=11)
            .map(|channel| 2407 + 5 * channel)
            .filter(|frequency| {
                *frequency >= self.channel_limits[0] && *frequency <= self.channel_limits[1]
            })
            .collect();
        report.record("passive scan channel count", frequencies.len() as u32);
        self.send_wmi(self.wmi_endpoint, &wmi::vdev_create(self.mac)?, report)?;
        self.send_wmi(
            self.wmi_endpoint,
            &wmi::scan_channels(&frequencies)?,
            report,
        )?;
        self.send_wmi(self.wmi_endpoint, &wmi::start_scan(&frequencies)?, report)?;
        let deadline = time::monotonic_ns().saturating_add(15_000_000_000);
        let mut networks: [Option<AccessPoint>; 20] = [None; 20];
        for _ in 0..4096 {
            let left = deadline.saturating_sub(time::monotonic_ns()) / 1_000_000;
            if left == 0 {
                return Err(Errno::ETIMEDOUT);
            }
            let event = self.next_wmi(left, report)?;
            if event.len() < 4 {
                continue;
            }
            let id = u32::from_le_bytes(event[..4].try_into().unwrap()) & 0xffffff;
            if id == 0x3001 && wmi::scan_complete(&event)? {
                report.record(
                    "passive scan networks",
                    networks.iter().flatten().count() as u32,
                );
                return Ok(networks);
            }
            if id == 0x7001 {
                if let Ok(frame) = wmi::management(&event) {
                    if let Ok(ap) = ieee80211::beacon(frame.frame, frame.frequency, frame.rssi) {
                        let index = networks
                            .iter()
                            .position(|old| old.is_some_and(|old| old.bssid == ap.bssid))
                            .or_else(|| networks.iter().position(Option::is_none));
                        if let Some(index) = index {
                            networks[index] = Some(ap);
                        }
                    }
                }
            }
        }
        Err(Errno::E2BIG)
    }
}
struct Bmi<'a> {
    controller: &'a mut Controller,
    report: &'a mut Report,
}
impl bmi::Transport for Bmi<'_> {
    fn exchange(&mut self, request: &[u8], reply: &mut [u8]) -> KResult<usize> {
        let terminal = crate::tty::Tty::new(osel_core::vfs::Stat::default());
        let mut byte = [0];
        if terminal.read(0, &mut byte, true) == Ok(1) && byte[0] == 3 {
            self.report.record("firmware load cancelled", 0);
            return Err(Errno::EINTR);
        }
        // Preserve command context on failure without flooding the screen for every chunk.
        let id = u32::from_le_bytes(request[..4].try_into().unwrap());
        let result = self.controller.bmi_exchange(request, reply, self.report);
        if result.is_err() {
            self.report.record("failed BMI command", id);
            if request.len() >= 8 {
                self.report.record(
                    "failed BMI argument",
                    u32::from_le_bytes(request[4..8].try_into().unwrap()),
                );
            }
        }
        result
    }
    fn pause(&mut self) -> KResult<()> {
        delay(10)
    }
    fn progress(&mut self, stage: &'static str, value: u32) {
        self.report.record(stage, value);
    }
}
struct Boot<'a> {
    controller: &'a mut Controller,
    report: &'a mut Report,
}
impl boot::Io for Boot<'_> {
    fn read32(&mut self, address: u32) -> KResult<u32> {
        let value = self.controller.diag(address, None, self.report)?;
        self.report.record("CE7 target value", value);
        Ok(value)
    }
    fn write(&mut self, address: u32, words: &[u32]) -> KResult<()> {
        self.controller
            .diag(address, Some(words), self.report)
            .map(|_| ())
    }
    fn wake_cpu(&mut self) -> KResult<()> {
        let value = self.controller.get(CORE) | 0x2000;
        self.controller.put(CORE, value);
        self.report
            .record("target CPU signalled", self.controller.get(CORE));
        Ok(())
    }
}
unsafe fn descriptor(ring: *mut u8, index: u32, address: u32, length: u16) {
    unsafe {
        let descriptor = ring.add(index as usize * 8);
        descriptor.cast::<u32>().write_volatile(address.to_le());
        descriptor
            .add(4)
            .cast::<u16>()
            .write_volatile(length.to_le());
        descriptor.add(6).cast::<u16>().write_volatile(0);
    }
}
impl Drop for Controller {
    fn drop(&mut self) {
        // Disable bus mastering before descriptor pages are freed, including timeout paths.
        self.device.bus_master(false);
        self.mask();
        if self.reset_on_drop {
            let reset = self.get(LOCAL + 8);
            unsafe { ((self.base + LOCAL + 8) as *mut u32).write_volatile(reset | 1) };
            let until = time::monotonic_ns().saturating_add(20_000_000);
            while time::monotonic_ns() < until {
                core::hint::spin_loop();
            }
            unsafe { ((self.base + LOCAL + 8) as *mut u32).write_volatile(reset & !1) };
            let until = time::monotonic_ns().saturating_add(20_000_000);
            while time::monotonic_ns() < until {
                core::hint::spin_loop();
            }
        } else if self.rings.is_some() {
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
fn initialize(report: &mut Report) -> KResult<Controller> {
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
    let chip = controller.get(RTC + 0xf0);
    report.record("SOC chip ID", chip);
    if (chip >> 8) & 0xf != 10 {
        return Err(Errno::EOPNOTSUPP);
    }
    report.record("previous firmware indicator", controller.get(INDICATOR));
    controller.reset(report)?;
    report.record("ROM early configuration begin", 0);
    boot::configure(&mut Boot {
        controller: &mut controller,
        report,
    })?;
    report.record("ROM early configuration complete", controller.get(CORE));
    controller.bmi_info(report)?;
    report.record("ROM ready; firmware not started", 0);
    Ok(controller)
}
fn probe(report: &mut Report, operation: &str) -> KResult<()> {
    let mut controller = initialize(report)?;
    if operation != "probe" {
        controller.load(report)?;
        if operation == "scan" {
            let results = controller.scan(report)?;
            *SCAN_RESULTS.lock() = results;
        }
        drop(controller);
        report.record("firmware check passed; target stopped", 1);
    }
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
            assert_eq!(command("load", &mut out), Err(Errno::ENODEV));
            assert_eq!(command("scan", &mut out), Err(Errno::ENODEV));
            assert!(!BUSY.load(Ordering::Acquire));
            assert_eq!(REPORT.lock().error, Some(Errno::ENODEV));
        }
    }
}
