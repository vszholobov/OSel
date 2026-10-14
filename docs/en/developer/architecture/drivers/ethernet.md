---
description: OSel driver directory, PCI enumeration and Intel 82574L DMA rings.
---

# PCI and Ethernet

**Audience:** hardware-driver developers.

**Summary:** hardware drivers live in `src/drivers/`: `avganec` console, `cereal` COM1, `keyboard` PS/2, `pci` and Intel 82574L `e1000e`. The first three remain re-exported from the library root, preserving names and macros. The stack is documented in [networking](../core/network.md).

## How it works {#how}

PCI configuration ports `0xcf8`/`0xcfc` enumerate 256 buses, 32 devices and functions 0–7 of multifunction devices. A 16-bit command write preserves adjacent write-one-to-clear status bits. BAR0 handles 32- and 64-bit addresses and rejects I/O BARs.

The driver accepts only Intel 82574L, `8086:10d3`. BAR0 must be below 4 GiB in the bootloader direct map; its huge page is made uncached. The driver enables memory access and bus mastering, waits for reset with a bound, reads the MAC address, and programs 32 RX and 32 TX legacy descriptors. Each buffer owns a physical page and uses its first 2048 bytes; each ring is contiguous within its own page.

Card IRQs are masked; the network thread services RX/TX. Incoming frames require DD/EOP, valid length and no receive errors. Invalid frames return to the ring without entering the stack. Checksum offloads are disabled. Volatile descriptor access and barriers order packet data and DMA ownership. TX never overwrites a busy descriptor.

## Code {#code}

- `src/drivers/mod.rs`: hardware-driver entry point.
- `src/drivers/pci.rs`: `Device`, `scan`, configuration ports.
- `src/drivers/e1000e.rs`: DMA pages, rings, MMIO and smoltcp `Device`.
- `osel-core/src/net.rs`: pure BAR and configuration-address decoding.

## Invariants and locks {#locks}

`PCI_CONFIG` covers the address/data pair. After initialization, `NETWORK` owns the adapter; no buffer references escape it. DMA pages survive while the adapter runs. Teardown disables RX/TX and masks IRQs before releasing pages. Unsupported adapters are left untouched.

## Pitfalls {#pitfalls}

- QEMU's `e1000e` name does not imply support for every Intel adapter: only one PCI ID is implemented.
- 10 ms polling is sufficient for the game. IRQ/MSI, multiple queues, other PHYs and power management are deferred.
- The owner must verify actual hardware. Successful PS/2 gameplay does not establish Ethernet compatibility.

## Tests {#tests}

PCI/BAR parsing is host tested. DHCP and DMA rings are exercised by [network tests](../core/network.md#tests) under UEFI and BIOS in QEMU.

_Checked against code: 2026-10-14, stage-4-network-tetris branch._
