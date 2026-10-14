---
description: Why hardware-independent kernel logic lives in osel-core and is tested on the host.
---

# 0002. osel-core and host tests

Language: [Русский](../../../ru/developer/decisions/0002-osel-core-host-tests.md) · [English](0002-osel-core-host-tests.md)

**Audience:** developers deciding where new logic belongs.

**Summary:** console rendering, allocators, memory maps, keyboard decoding, exception reports and locking live in `osel-core/`. `cargo test-core` tests them on the host in fractions of a second. Ports, assembly and globals remain in `src/`.

**Status:** accepted. **Date:** 2026-10-11.

## Context

Originally every test required an image build and QEMU boot. Edge cases were expensive, and the development host was an ARM Mac. Reviews found ordinary logic errors in overlapping regions, address overflow and console scrolling that host unit tests can detect.

## Decision

The workspace contains the kernel and `osel-core`, excluding `runner`. `osel-core` is `no_std` outside tests and uses `x86_64` without its `instructions` feature, allowing host builds. `.cargo/config.toml` provides `cargo test-core` and `cargo test-runner`; CI has a fast job without QEMU. Interrupts, page tables, real framebuffer access and crashes remain QEMU tests.

## Alternatives considered

QEMU-only testing is slow and awkward for edge cases; mutation testing exposed coverage gaps. `per-package-target` works on nightly but is unstable and poorly supported by IDEs.

## Consequences

New logic normally belongs in `osel-core` with tests, integrated by thin kernel code. It cannot access ports or CPU registers. See [testing architecture](../architecture/testing.md).
