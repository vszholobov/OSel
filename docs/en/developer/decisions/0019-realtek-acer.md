---
description: Decision 0019 — Realtek for Acer Nitro and separate validation of DMA and the physical PHY.
---

# 0019 — Realtek for Acer Nitro

**Status:** accepted. **Date:** 2026-10-14. Extends [0018](0018-networking.md).

## Context

The owner reported Realtek Gaming GBE Family Controller and hardware ID `PCI\VEN_10EC&DEV_8168&SUBSYS_13661025&REV_25`. Networking already works with Intel 82574L in QEMU. Samsung is outside the task. Standard QEMU does not emulate RTL8168; PCI revision does not identify its internal MAC variant.

## Decision

Add `rtl8168` to the driver directory and dispatch Intel/Realtek through `ethernet`, preserving TCP and Linux ABI. Identify MACs from TxConfig XID and restrict the gigabit path to known G/GU/H/M variants. Diagnose unknown variants without starting DMA. Implement basic OCP PHY startup, auto negotiation, polling and rings without offload. Built-in PHY firmware remains unchanged; firmware replacement and analog tuning are outside the initial driver.

Use PIT channel 2 for bounded hardware delays with interrupts disabled, preserving scheduler channel 0. Enable bus mastering after ring addresses are installed and disable it before freeing pages. Test PHY startup through a host model and shared DMA through end-to-end gameplay using QEMU Realtek C+. `OSEL_NIC` selects emulation or boot without a card.

## Alternatives

- Treat every `10ec:8168` alike: PHYs and MAC modes differ.
- Claim C+ fully emulates Acer: it validates rings, not the gigabit PHY.
- Develop RTL8168 emulation in QEMU: a separate large project that still cannot establish physical connectivity.
- Immediately import a third-party driver and firmware: a much larger scope requiring separate licensing and hardware analysis.

## Consequences

Existing Intel tests remain, while Realtek has separate UEFI and BIOS checks. Acer's PCI ID is confirmed, but XID, PHY startup and connectivity remain a hardware checkpoint. Its results determine whether additional PHY tuning or firmware is needed. See [Ethernet](../architecture/drivers/ethernet.md).
