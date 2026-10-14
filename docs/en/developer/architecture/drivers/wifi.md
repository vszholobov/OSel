---
description: "Initial QCA6174 driver: PCI, power, reset, Copy Engine, BMI and diagnostic snapshots."
---

# QCA6174 Wi-Fi

**Audience:** hardware and firmware bring-up developers.

**Summary:** a hardware probe targets Acer `168c:003e`, subsystem `11ad:0807`, revision `32`. `wifi probe` attempts BMI Target Info after PCI/D0/wake, cold/warm reset and CE0/CE1/CE7 setup and ROM early configuration. Bundled firmware is validated but not started. Scanning, association, WPA2 and the packet data path are not implemented.

## How it works {#how}

Only a shell command starts probing. Exact PCI IDs/revision and firmware/board containers are checked first. BAR0 must be 1 MiB aligned with the required span below 4 GiB. Direct-map MMIO pages become uncached. The adapter enters D0 without ASPM/CLKREQ; SOC_WAKE is asserted and RTC=ON is checked with a deadline.

QCA6174 uses cold reset, target-initialized waiting and warm reset. A global-reset write is followed by 20 ms without PCIe reads. PCI command disables INTx and CE interrupts are masked. ROM waiting repeats the PCIe core event-enable write to address a register-mapping race, while host INTx stays disabled. Missing/crashed targets return errors rather than waiting forever.

Six zeroed DMA pages below 4 GiB hold CE0 source, CE1 destination, request, response and both diagnostic CE7 rings. CE7 has two entries; it reads `hi_interconnect_state` and writes target pipe config, the service map, L1 disablement and early allocation for the nine IRAM banks of hw3.2. After setting and reading back `HI_OPTION_EARLY_CFG_DONE`, PCIe core control signals the target CPU. Only then is BMI attempted; `target initialized=2` alone does not mean BMI is ready. Target RAM pointers are checked for alignment and bounds before DMA. Host setup preserves the ROM-owned halves of CE0/CE1. The BMI probe uses 16-entry rings with 8-byte descriptors and publishes one request. BMI GET_TARGET_INFO returns 12 bytes: size, target version and type. Only version `0x05030000` is accepted currently; another version is recorded and returns `EOPNOTSUPP`. BMI_DONE is not sent and firmware code is not downloaded.

Before freeing DMA, PCI bus mastering is disabled, events are masked, rings are halted and their sizes cleared. Timeout paths use the same teardown. The probe is not attached to smoltcp and does not change Ethernet. Subsequent Wi-Fi bring-up needs reinitialization.

## Firmware and board {#firmware}

`firmware/` contains unmodified `firmware-6.bin`, `board-2.bin`, license and required notice. `firmware/manifest.json` records source, date and SHA256; `tools/check-firmware.py` verifies exact bytes in CI. Board selection requires the exact LiteOn `11ad:0807` PCI entry and 8124 bytes, without fallback to another board's calibration.

The parser checks NUL-terminated magic, 4-byte alignment, nested TLVs, lengths and overflow. Duplicate mandatory fields are rejected. Bundled firmware contains WMI TLV=4, HTT=3 and OTP. `wifi fw` reports version and sizes; bundled does not mean running.

The runner always includes `/usr/share/licenses/ath10k/LICENSE` and `/usr/share/licenses/ath10k/NOTICE` and the adapted driver license `DRIVER-ISC`, including with custom `OSEL_RAMDISK`. These paths are reserved; replacement or a file occupying a parent directory causes a packing error. This requires no program installation on the owner's machine.

## Code {#code}

- `src/drivers/qca6174.rs`: hardware probe, bounded waits and last-stage snapshot.
- `src/drivers/pci.rs`, `src/drivers/dma.rs`: PCI and DMA pages.
- `osel-core/src/wifi.rs`: embedded files and checked container parser.
- `osel-core/src/wifi/boot.rs`: ROM early configuration order, ABI tables, address checks and a testable CE7 interface.
- `src/diagnostics.rs`, `src/shell.rs`: report and commands; [user diagnostics](../../../user/diagnostics.md).
- `runner/src/main.rs`: mandatory license files in images.

## Locks and invariants {#locks}

Atomic BUSY permits one probe. Its guard releases the flag on all ordinary returns. `WIFI_REPORT` protects only a copyable 64-record report; no sleeping, MMIO or printing occurs under it. The caller owns the probe; waits require enabled IRQs and must not run in an IRQ handler. `NETWORK` and `WIFI_REPORT` are not nested.

Each attempt clears the report first. The last completed stage and errno remain after the command returns. `wifi regs` shows the last 22 saved entries to fit the display, without new register reads. Before timeout cleanup it captures PCI command/BME, PCIe core control, ring bases/sizes, control/command, read/write indices and errors for the failing CE. Stages are shown on the display and sent to COM1 before the whole probe finishes.

## Pitfalls {#pitfalls}

- QEMU does not emulate QCA6174. Green CI validates parsing, absent-card behavior and cleanup, not Acer PCIe/reset/CE.
- Without early config and CPU signalling, ROM may report initialized without consuming BMI. Never clear ROM-owned rings.
- PCI revision does not replace BMI target version; continued bring-up needs a hardware report.
- Enable bus mastering only after valid rings are published; never free active DMA.
- Never wait under `IrqSpinlock`: losing PIT turns a bounded timeout into a hang.
- Firmware API 6 differs from the HTT packet interface. Bundled files do not provide WPA2 or DHCP.

## Tests {#tests}

`cargo test-core` checks actual embedded containers, exact board selection, truncation, bad lengths/magic and duplicates. A kernel test checks absent-card behavior, BUSY release and cleanup on repeated probes. `tests/shell.rs` and `tools/check-shell.py` cover commands, reports and image licenses. On 2026-10-15 the owner confirmed PCI, RTC `0x73`, chip ID `0x00340aff` and three initialized `2` waits on Acer using image `a53f703`. CE0 did not consume the first BMI request (read index `0`, error status `0`, `ETIMEDOUT`); the shell stayed responsive. This exposed missing ROM early configuration and CPU signalling. The new CE7 path and BMI response still need another Acer check. ROM model tests cover configuration before CPU signalling, preserved flags, corrupt pointers and DMA timeouts without signalling readiness.

Next steps: [Wi-Fi design](../../design/wifi.md). Checked against code: 2026-10-15, branch `wifi-qca6174-diagnostics`.
