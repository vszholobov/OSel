---
description: OSel failure messages and symptoms, with causes and next debugging steps.
---

# Troubleshooting

**Audience:** developers investigating a failed test, CI job or boot.

**Summary:** find the message below, then follow the linked subsystem. For an unknown symptom, start with [debugging](debugging.md). Runner prints `failed: …`; panic contents come from the kernel.

## Messages {#messages}

| Message | Meaning and action |
|---|---|
| ``deadlock on lock `X`: held since A, waiting at B`` | Recursive or unreleased lock. Check both locations, nested memory/allocator calls and exception printing; see [locking](../architecture/core/locking.md) |
| `the test leaked N heap bytes` | Release all allocations; see [kernel tests](testing.md#kernel) |
| `the test leaked N frames` | Release pages/page tables with `deallocate_frame`, `unmap_range` or `prune_empty_tables` |
| `frame … is not handed out: double free or a foreign frame` | Invalid frame ownership; see [pool](../architecture/core/memory.md#frames) |
| `osel::init called twice` | Initialize once; use `osel::boot_info()` afterward |
| `failed: qemu exit code Some(35)` | Kernel reported failure; read preceding `[failed]` text |
| `failed: qemu exit code Some(0)` or `Some(1)` | QEMU did not use the exit device, often a [triple fault](debugging.md#triple-fault) |
| `failed: N of M tests reported [ok]` | Early `exit_qemu` or output mixed into another line |
| `failed: no test reported [ok]` | No check completed, or a harness-free test forgot `pass()` |
| `failed: timed out after 60s` | Hang; inspect the final test, or temporarily raise `OSEL_TIMEOUT` on a slow machine |
| `failed: … not printed within 60s` | Expect mode did not see the text; print to serial, not just framebuffer |
| `failed: kernel stopped with … before printing …` | Early panic/reset; inspect [debugging](debugging.md) |
| `unknown OSEL_BOOT value …` | Use only `uefi` or `bios` |
| `OSEL_TIMEOUT must be a positive number of seconds, got …` | Set a positive integer |
| `failed to fetch OVMF firmware` | First UEFI run needs network; cache is `runner/.ovmf`, configurable with `OSEL_OVMF_DIR` |
| `warning: test arguments are not supported and ignored` | Name filters after `--` do not work; use `cargo test --test <name>` |

## Symptoms {#symptoms}

| Symptom | Cause and action |
|---|---|
| CI `test_timer_frequency_against_rtc` reports `N ticks per second, expected 100` | Software-emulated QEMU can miss ticks under load; test accepts 25–400; see `src/interrupts.rs` and [interrupts](../architecture/core/interrupts.md) |
| Only UEFI or BIOS fails | Firmware initializes devices/memory differently; test both |
| Build fails after nightly update | Roll back `rust-toolchain.toml`; see [toolchain](toolchain-and-ci.md) |
| Only weekly latest-nightly CI fails | Pinned compiler still works; investigate before updating |
| Laptop keyboard is silent | External USB may need legacy emulation; try built-in PS/2 and inspect [keyboard](../architecture/drivers/keyboard.md) |
| USB absent from boot menu | Check Fast BIOS Mode, Secure Boot and image mode; see [USB boot](../../user/usb.md) |
| Red screen | Save a [crash report](../../user/crash-report.md) |
| Black screen or reboot without report | Early failure or [triple fault](debugging.md#triple-fault) |
