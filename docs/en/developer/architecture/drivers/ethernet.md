---
description: Intel 82574L and Realtek RTL8168 drivers, PCI, PHY startup and DMA rings in OSel.
---

# PCI and Ethernet

**Audience:** hardware-driver developers.

**Summary:** drivers live in `src/drivers/`. Networking uses Intel 82574L or Realtek RTL8168 G/GU/H/M through `ethernet`. Realtek DMA is tested using QEMU C+; the Acer PHY requires a laptop boot. `avganec`, `cereal` and `keyboard` remain re-exported from the library root. The stack is documented in [networking](../core/network.md).

## How it works {#how}

PCI ports `0xcf8`/`0xcfc` enumerate 256 buses, 32 devices and functions 0–7 of multifunction devices. Word command writes leave adjacent write-one-to-clear status untouched. Realtek selects the first memory BAR, skipping I/O BARs and the upper half of 64-bit BARs. Capability traversal is bounded and rejects repeated pointers. The adapter enters D0; ASPM and CLKREQ are disabled. Intel uses BAR0.

Both drivers require a BAR below 4 GiB in the bootloader direct map. The MMIO huge page becomes uncached. Each of the two 32-descriptor rings occupies one physical page; each buffer owns a page and uses its first 2048 bytes. Shared `dma` zeroes pages and returns them to the frame pool on teardown.

### Intel {#intel}

`e1000e` accepts Intel 82574L, `8086:10d3`. It waits for reset with a bound, reads the MAC and programs legacy rings. RX validates DD/EOP, length and error flags; TX never overwrites a busy descriptor. Polling does not depend on IRQ/MSI; offloads are disabled.

### Realtek {#realtek}

`rtl8168` accepts `10ec:8168` only for known XIDs: `4c0` (G), `509` (GU), `541` (H) or `6c0` (M). TxConfig provides the XID; PCI revision does not select initialization. The owner supplied `PCI\VEN_10EC&DEV_8168&SUBSYS_13661025&REV_25` for Acer, but its XID has not been read. Unknown XIDs produce diagnostics and boot continues without networking.

For G/GU/H/M the driver powers up PHY/PLL, accesses standard PHY registers through GPHY OCP, bounds the reset wait and restarts 10/100/1000 auto negotiation. It does not replace built-in PHY firmware or implement chip-specific analog tuning and firmware corrections. Whether Acer needs these requires hardware testing. Rings are configured, jumbo/checksum/VLAN/segmentation offloads disabled, and the RXDV gate opened before enabling MACs. Bus mastering starts only after every DMA address is installed.

Descriptors contain two 32-bit control words and a 64-bit physical buffer address. OWN transfers ownership to hardware, EOR marks the last slot, FIRST/LAST require a complete frame. RX validates lengths and errors, strips four FCS bytes and returns descriptors with the original EOR. TX zeroes padding to the 60-byte minimum. Volatile accesses and barriers order buffers and ownership. Malformed frames increment `dropped`.

`10ec:8139` is accepted only for XID `748`, meaning C+. QEMU exercises the same rings with different TX doorbell, RX flags and enable command. C+ lacks the Acer gigabit PHY; successful emulated gameplay does not prove laptop PHY startup.

Register facts were checked against primary [FreeBSD re](https://github.com/freebsd/freebsd-src/blob/main/sys/dev/re/if_re.c), [Linux r8169](https://github.com/torvalds/linux/blob/master/drivers/net/ethernet/realtek/r8169_main.c) and [QEMU rtl8139](https://github.com/qemu/qemu/blob/master/hw/net/rtl8139.c) sources. Third-party implementations are not imported.

## Code {#code}

- `src/drivers/mod.rs`: visible hardware-driver entry point.
- `src/drivers/pci.rs`: `Device`, `scan`, BAR, D0, ASPM and bus mastering.
- `src/drivers/dma.rs`: physical pages shared by both drivers.
- `src/drivers/ethernet.rs`: adapter selection and smoltcp `Device` tokens.
- `src/drivers/e1000e.rs`: Intel legacy DMA and MMIO.
- `src/drivers/rtl8168.rs`: Realtek DMA, MMIO and PIT channel 2 delays.
- `osel-core/src/rtl8168.rs`: XID, descriptor validation, PHY/OCP and host-tested PHY model.
- `osel-core/src/net.rs`: pure BAR and PCI-address decoding.

## Invariants and locks {#locks}

`PCI_CONFIG` covers address/data pairs. After initialization `NETWORK` owns the adapter; no buffer references escape it. DMA pages survive while the card runs. Realtek masks IRQs, disables RX/TX, closes the RXDV gate and bus mastering, and performs a bounded reset before releasing pages. Allocation failures free previous pages without enabling DMA.

`PIT2` protects channel 2 and port `0x61` gate/speaker controls; scheduler channel 0 is unchanged. Bounded polling works with interrupts disabled during boot. Previous gate/speaker controls are restored, but channel 2 belongs to this driver: future users must coordinate through the lock. Delays do not depend on CPU speed. `PIT2` takes no other locks; teardown may nest `NETWORK` → `PCI_CONFIG`/`PIT2`/`MEMORY`.

## Pitfalls {#pitfalls}

- `10ec:8168` combines several MACs: a PCI ID alone cannot establish support. Unknown XIDs and PHY timeouts appear on framebuffer and COM1 during boot.
- Standard QEMU does not emulate RTL8168. C+ tests DMA; a host model tests G/GU/H/M PHY initialization. Physical hardware remains untested.
- IRQ/MSI, other Realtek MACs, Wi-Fi, USB Ethernet, PHY firmware and power saving are absent. Samsung is outside this task.
- `net` reports the adapter, link, addresses, RX/TX and dropped counters. A cable without DHCP does not provide an IPv4 address.

## Tests {#tests}

`cargo test-core` covers unknown XIDs, OWN, EOR, fragmentation, lengths/FCS and RX errors, and exercises the actual PHY startup procedure through an OCP model, including access, reset and timer failures. `cargo test-runner` checks `OSEL_NIC` selection.

`tests/realtek_dma.rs` creates the secondary QEMU adapter three times, obtains DHCP, stops DMA and frees rings/buffers; bus mastering must be off and heap/pages returned. `tests/realtek_network.rs` repeats HTTP/WebSocket, a large TCP stream and two-client gameplay through Realtek. `tests/realtek_network_game.rs` drives the actual client, receives both fields and pong. All run under UEFI and BIOS. `OSEL_NIC=rtl8139 python3 tools/check-network.py` checks the real shell, two games and offline-server recovery.

## Decisions {#decisions}

- [0018: Ethernet and outgoing TCP sockets](../../decisions/0018-networking.md).
- [0019: Realtek for Acer and emulation limits](../../decisions/0019-realtek-acer.md).

_Checked against code: 2026-10-14, stage-4-network-tetris branch._
