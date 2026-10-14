---
description: Why Rust nightly is pinned and the latest compiler is checked weekly.
---

# 0004. Pinned nightly

**Audience:** developers updating the toolchain or investigating build failures.

**Summary:** `rust-toolchain.toml` pins a nightly date. Weekly CI checks the latest nightly; a green result allows a deliberate update using the [toolchain procedure](../guides/toolchain-and-ci.md).

**Status:** accepted. **Date:** 2026-10-11.

## Context

The kernel needs `custom_test_frameworks`, `abi_x86_interrupt` and bootloader-stage builds. Nightly changes have broken target JSON formats and `x86_64` 0.14. A floating version could break unrelated PRs and invalidate CI caches daily.

## Decision

Pin `channel` to a date. Weekly and manual “latest nightly” jobs set `RUSTUP_TOOLCHAIN=nightly` and run `cargo test-core`, `cargo test-runner` and `cargo test --no-fail-fast` in QEMU. They do not check documentation or boot the normal kernel.

## Alternatives considered

Floating nightly risks unrelated failures and cold caches. Stable Rust cannot supply the unstable kernel features.

## Consequences

Toolchain updates are separate, intentional changes. Future compiler failures are visible before they affect regular work.
