---
description: Why OSel uses bootloader 0.11 for both UEFI and BIOS with the current BootInfo interface.
---

# 0001. bootloader 0.11 and UEFI boot

**Audience:** developers changing boot or BootInfo handling.

**Summary:** `bootloader` 0.11 supports UEFI and BIOS. Text uses the framebuffer instead of VGA; `runner/` builds disk images instead of `bootimage`.

**Status:** accepted. **Date:** 2026-10-11.

## Context

OSel targets real laptops. `bootloader` 0.9 only supports BIOS; many modern computers lack CSM and require UEFI. The old `x86_64` 0.14 crate also stopped compiling on recent nightly Rust.

## Decision

- Use `bootloader` 0.11: one kernel ELF, two images, `OSel.uefi.img` and `OSel.bios.img`.
- Build for the built-in `x86_64-unknown-none` target without a custom JSON target.
- Render text to the framebuffer using `osel-core/src/console.rs` and `src/drivers/avganec.rs`; UEFI has no VGA text mode.
- Build images and launch QEMU using `runner/src/main.rs`, connected to Cargo through `runner/run.sh`.
- Run CI tests in both boot modes.

## Alternatives considered

Staying on `bootloader` 0.9 would require fewer changes but would fail on UEFI-only hardware. Writing a bootloader adds work already covered by 0.11. Limine remains a possible future choice for a boot menu alongside Windows.

## Consequences

Memory and `BootInfo` handling use the new memory-map and physical-offset interfaces. The bootloader leaves its SS selector and low-half pages behind; see [interrupts](../architecture/core/interrupts.md) and [memory](../architecture/core/memory.md). The runner downloads OVMF for UEFI in QEMU.
