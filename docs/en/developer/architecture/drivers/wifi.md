---
description: "Initial QCA6174 driver: PCI, power, reset, Copy Engine, BMI and diagnostic snapshots."
---

# QCA6174 Wi-Fi

**Audience:** hardware and firmware bring-up developers.

**Summary:** a hardware probe targets Acer `168c:003e`, subsystem `11ad:0807`, revision `32`. `wifi probe` attempts BMI Target Info after PCI/D0/wake, cold/warm reset and CE0/CE1/CE7 setup and ROM early configuration. `wifi load` also uploads firmware and checks HTC/WMI ready, then stops the target. `wifi scan` implements passive 2.4 GHz AP discovery; firmware/scanning still needs hardware validation. Association, the WPA2 handshake and the packet data path are not implemented.

## How it works {#how}

Only a shell command starts probing. Exact PCI IDs/revision and firmware/board containers are checked first. BAR0 must be 1 MiB aligned with the required span below 4 GiB. Direct-map MMIO pages become uncached. The adapter enters D0 without ASPM/CLKREQ; SOC_WAKE is asserted and RTC=ON is checked with a deadline.

QCA6174 uses cold reset, target-initialized waiting and warm reset. A global-reset write is followed by 20 ms without PCIe reads. PCI command disables INTx and CE interrupts are masked. ROM waiting repeats the PCIe core event-enable write to address a register-mapping race, while host INTx stays disabled. Missing/crashed targets return errors rather than waiting forever.

Ten zeroed DMA pages below 4 GiB hold CE0 source, CE1 destination, request, response and both diagnostic CE7 rings, a CE2 ring with its own WMI RX buffer, and CE3/CE4 TX rings. CE7 has two entries; it reads `hi_interconnect_state` and writes target pipe config, the service map, L1 disablement and early allocation for the nine IRAM banks of hw3.2. After setting and reading back `HI_OPTION_EARLY_CFG_DONE`, PCIe core control signals the target CPU. Only then is BMI attempted; `target initialized=2` alone does not mean BMI is ready. Target RAM pointers are checked for alignment and bounds before DMA. Host setup preserves the ROM-owned halves of CE0/CE1. The BMI probe uses 16-entry rings with 8-byte descriptors and publishes one request. BMI GET_TARGET_INFO returns 12 bytes: size, target version and type. Only version `0x05030000` is accepted currently; another version is recorded and returns `EOPNOTSUPP`. `wifi probe` stops without BMI_DONE; `wifi load` continues as described below.

`wifi load` uses BMI commands of at most 256 bytes: EFUSE-selected PLL, host-interest/endianness, exactly 8124 bytes of the selected board, OTP and the main SGMT image through an LZ stream. Its last word is zero-padded and a zero-address stream flushes ROM caches. OTP status respects the bundled firmware IGNORE_OTP_RESULT feature. OTP execution calibrates the target; no flash/OTP programming commands are used. CE1 RX is posted before BMI_DONE; HTC ready is checked for endpoint, lengths, message ID, credits and credit size. HTC then connects HTT/WMI services and sends SETUP_COMPLETE_EX. CE2 receives WMI service-ready; ABI, RF chains, services and bounded memory requests are checked. INIT advertises only owned, zeroed memory below 4 GiB; unit counts match vdev/peer resources. Limits are 1 MiB per request and 8 MiB in total. WMI ready requires matching ABI, successful status and a valid MAC. This does not establish association or connectivity. Progress appears every 64 KiB; a 120-second deadline follows the ROM check, with Ctrl+C cancellation.

Before freeing DMA, PCI bus mastering is disabled, events are masked, rings are halted and their sizes cleared. Timeout paths use the same teardown. After `wifi load`, cold reset stops the target CPU before freeing DMA, including error and cancellation paths. The probe is not attached to smoltcp and does not change Ethernet. Subsequent Wi-Fi bring-up needs reinitialization.

`wifi scan` creates a station vdev and passive channels 1–11 within factory limits, then requires a successful scan-completed event. WMI events use bounded queues; HTC trailers from both CE1 and CE2 replenish credits, and TX buffers are not reused before DMA completes. Beacon/probe-response parsing checks SSID/BSSID, IE/RSN lengths, CCMP/PSK and mandatory PMF. Malformed frames are skipped and AP storage is capped at 20. SSID control characters are escaped. Results are copied from WIFI_SCAN before output. No association occurs; the target stops after the command.

## Firmware and board {#firmware}

`firmware/` contains unmodified `firmware-6.bin`, `board-2.bin`, license and required notice. `firmware/manifest.json` records source, date and SHA256; `tools/check-firmware.py` verifies exact bytes in CI. Board selection requires the exact LiteOn `11ad:0807` PCI entry and 8124 bytes, without fallback to another board's calibration.

The parser checks NUL-terminated magic, 4-byte alignment, nested TLVs, lengths and overflow. Duplicate mandatory fields are rejected. Bundled firmware contains WMI TLV=4, HTT=3 and OTP. `wifi fw` reports version and sizes; bundled does not mean running.

The runner always includes `/usr/share/licenses/ath10k/LICENSE` and `/usr/share/licenses/ath10k/NOTICE` and the adapted driver license `DRIVER-ISC`, including with custom `OSEL_RAMDISK`. These paths are reserved; replacement or a file occupying a parent directory causes a packing error. This requires no program installation on the owner's machine.

## Code {#code}

- `src/drivers/qca6174.rs`: hardware probe, bounded waits and last-stage snapshot.
- `src/drivers/pci.rs`, `src/drivers/dma.rs`: PCI and DMA pages.
- `osel-core/src/wifi.rs`: embedded files and checked container parser.
- `osel-core/src/wifi/ieee80211.rs`: beacon, SSID and RSN.
- `osel-core/src/wifi/wmi.rs`: WMI TLVs, service-ready/ready and INIT encoding.
- `osel-core/src/wifi/bmi.rs`, `osel-core/src/wifi/htc.rs`: upload protocol and checked first firmware response.
- `osel-core/src/wifi/boot.rs`: ROM early configuration order, ABI tables, address checks and a testable CE7 interface.
- `src/diagnostics.rs`, `src/shell.rs`: report and commands; [user diagnostics](../../../user/diagnostics.md).
- `runner/src/main.rs`: mandatory license files in images.

## Locks and invariants {#locks}

Atomic BUSY permits one probe. Its guard releases the flag on all ordinary returns. `WIFI_REPORT` protects only a copyable 64-record report; no sleeping, MMIO or printing occurs under it. The caller owns the probe; waits require enabled IRQs and must not run in an IRQ handler. `NETWORK` and `WIFI_REPORT` are not nested.

Each attempt clears the report first. The last completed stage and errno remain after the command returns. `wifi regs` shows the last 22 saved entries to fit the display, without new register reads. Before timeout cleanup it captures PCI command/BME, PCIe core control, ring bases/sizes, control/command, read/write indices and errors for the failing CE. `CE error status` masks `0x7e0`: the low bit of the raw register is not a DMA error. Stages are shown on the display and sent to COM1 before the whole probe finishes.

## Pitfalls {#pitfalls}

- QEMU does not emulate QCA6174. Green CI validates parsing, absent-card behavior and cleanup, not Acer PCIe/reset/CE.
- Without early config and CPU signalling, ROM may report initialized without consuming BMI. Never clear ROM-owned rings.
- PCI revision does not replace BMI target version; continued bring-up needs a hardware report.
- Enable bus mastering only after valid rings are published; never free active DMA.
- Never wait under `IrqSpinlock`: losing PIT turns a bounded timeout into a hang.
- Firmware API 6 differs from the HTT packet interface. Bundled files do not provide WPA2 or DHCP.

## Tests {#tests}

`cargo test-core` checks actual embedded containers, exact board selection, truncation, bad lengths/magic and duplicates. A kernel test checks absent-card behavior, BUSY release and cleanup on repeated probes. `tests/shell.rs` and `tools/check-shell.py` cover commands, reports and image licenses. On 2026-10-15 the owner confirmed PCI, RTC `0x73`, chip ID `0x00340aff` and three initialized `2` waits on Acer using image `a53f703`. CE0 did not consume the first BMI request (read index `0`, error status `0`, `ETIMEDOUT`); the shell stayed responsive. This exposed missing ROM early configuration and CPU signalling. The owner confirmed CE7 and BMI with image `8a7c625`: 12-byte response, version `0x05030000`, type `8`, error=None. The new firmware/HTC/WMI path still needs Acer validation. ROM model tests cover configuration before CPU signalling, preserved flags, corrupt pointers and DMA timeouts without signalling readiness.

`DmaRegion` in `src/drivers/dma.rs` allocates contiguous pages by retaining a bounded prefix of LIFO candidates until it finds a consecutive run; unused candidates are returned on success or failure. Vec capacity is allocated before MEMORY; no heap allocation or sleeping occurs under it. A kernel test repeats allocation after reverse-order freeing and checks zeroed bytes, contiguity, the 4 GiB boundary and leak-free teardown. Firmware stops before freeing these regions.

The BMI model covers real firmware/OTP/board bytes, command order, all eight reference clocks, preserved PLL bits, chunk boundaries/padding, timeouts, short replies and duplicate BMI_DONE rejection. HTC/WMI tests cover truncated/duplicate TLVs, ABI, endpoint/service/status, request sizes and ready events. INIT tests check that resources and supplied memory agree.

Next steps: [Wi-Fi design](../../design/wifi.md). Checked against code: 2026-10-15, branch `wifi-qca6174-diagnostics`.
