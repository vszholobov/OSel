---
description: Host/runner/QEMU test levels, testosterone framework, expected panics, leak checks and success criteria.
---

# Testing architecture

**Audience:** developers adding or repairing tests.

**Summary:** pure kernel logic and runner use fast host tests; hardware tests are kernels booted in QEMU with both firmware modes. Serial text and QEMU exit codes report results. Framework tests check heap/frame leaks. See [test recipes](../guides/testing.md).

## Three levels {#levels}

| Level | Command | Location | Coverage |
|---|---|---|---|
| Core logic | cargo test-core | `osel-core/src/`, #[test] | Rendering, allocation, memory maps, keyboard, reports and locks |
| Runner | cargo test-runner | `runner/src/main.rs`, #[test] | Output, status and timeout |
| Kernel | cargo test | `src/` #[test_case], `tests/` | CPU/devices, paging and faults |

`.cargo/config.toml` aliases override the default x86_64-unknown-none target with the host tuple.

## QEMU kernels {#kernel}

Module tests share the library test kernel with test_kernel_main in `src/lib.rs`. Every integration file is a separate kernel; custom-main kernels need harness=false in Cargo.toml. Cargo invokes runner/run.sh to package the common [ramdisk](core/fs.md) and /bin programs and boot with isa-debug-exit. exit_qemu in `src/qemuno.rs` writes 0x10/0x11 to port 0xf4, producing QEMU status 33/35. See [runner](../reference/runner.md).

## testosterone framework {#testosterone}

`src/testosterone.rs` replaces the unavailable no_std test harness. test_runner prints Running N tests, runs each named test with trailing [ok], then exits successfully. test_panic_handler prints [failed]/panic text and fails. If serial was locked, force_unlock then use serial_println!; only nested panic uses emergency_print.

### Leak checks {#leaks}

Testable::run compares heap in_use from `osel-core/src/heap.rs` before/after each test; cached blocks are excluded. Difference fails with the test leaked N heap bytes. forbid_frame_leaks captures memory::used_frames: excess gives the test leaked N frames; a lower count gives the test freed N frames it did not allocate. Use an extra guard around repeated failing allocation attempts when needed. Tests free their own scratch tables.

Only #[test_case] gets automatic checks. harness=false/panic_test! may call forbid_frame_leaks explicitly but do not automatically check leaks. `tests/leak_detection.rs` and `tests/frame_leak_detection.rs` verify detection.

### Expected panics {#panic-test}

panic_test! creates a kernel that succeeds only when body panic contains every expected fragment. expect_panic shows the screen, rejects nested panic with emergency output, formats through heap-free FixedBuf, checks text and optional EXPECTED_RIP, then verifies the red bottom-left pixel, OSel crashed :( first row and version/commit second row. Without framebuffer, skip screen checks. Assembly sets EXPECTED_RIP immediately before a fault; see `tests/invalid_opcode.rs` and `tests/divide_error.rs`.

## Passing a test {#pass}

Runner requires the success exit code plus exactly the announced count of [ok] lines, or at least one for harness-free kernels. See [exact rules](../reference/runner.md#pass).

## CI {#ci}

See [toolchain/CI](../guides/toolchain-and-ci.md). CI QEMU uses software emulation without KVM; timing tests accept wide variance and KVM support is [deferred](../debt.md).

## Future verification {#future-checks}

Two stage-2 plans remain: compare the same ELF output/status against Linux (also capture Go strace -f), and add an -icount shift=N,align=off,sleep=off runner mode for instruction-based time. Neither is automated in current CI; both are in debt.

## Pitfalls {#pitfalls}

Filters after cargo test -- do not work; select an integration kernel with --test. Cargo puts test ELF files in `target/x86_64-unknown-none/debug/build/osel/<hash>/out`, not deps; runner detects the 16-hex-digit suffix. Tests in one kernel share console/cursor, frame pool and input state. Leak-check panic appears after the test name, so it resembles a body failure.

## Tests {#tests}

`tests/leak_detection.rs` checks actual leak detection, `tests/nested_panic.rs` checks nesting, and runner host tests check success criteria.

## Decisions {#decisions}

[0002](../decisions/0002-osel-core-host-tests.md).

_Checked against code: 2026-10-12, commit 8a4d3e2._
