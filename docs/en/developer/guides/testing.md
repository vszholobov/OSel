---
description: Add host, kernel, integration and expected-panic tests, then run them in both boot modes.
---

# Add and run tests

Language: [Русский](../../../ru/developer/guides/testing.md) · [English](testing.md)

**Audience:** developers changing code.

**Summary:** test pure logic in `osel-core` with `#[test]`; test hardware with kernel `#[test_case]` or integration kernels in `tests/`. Use `osel::panic_test!` for expected failures and check both boot modes. See [testing architecture](../architecture/testing.md).

## Choose a test {#choose}

| Subject | Location | Command |
|---|---|---|
| Allocators, parsing, formatting, in-memory console | `osel-core/src/<module>.rs` | `cargo test-core` |
| Runner output and exit codes | `runner/src/main.rs` | `cargo test-runner` |
| Actual CPU and devices | Kernel module, `#[test_case]` | `cargo test` |
| Custom initialization or panic handler | Separate kernel in `tests/` | `cargo test` |
| Expected panic | `tests/`, `osel::panic_test!` | `cargo test` |

Move hardware-independent logic to `osel-core` whenever possible.

## Host test {#host}

Add a `#[test]` function inside `#[cfg(test)] mod tests` at the end of the module. Tests can use `std`, unlike normal crate code. Run `cargo test-core`.

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_buffer_has_no_items() {
        let mut buffer = RingBuffer::<u8, 4>::new(0);
        assert_eq!(buffer.pop(), None);
    }
}
```

## Kernel test {#kernel}

Add `#[test_case]` to a kernel-module test. Heap, interrupts and console are already initialized. `cargo test` prints each test name followed by `[ok]`.

```rust
#[cfg(test)]
mod tests {
    #[test_case]
    fn test_timer_ticks() {
        let start = crate::interrupts::ticks();
        while crate::interrupts::ticks() < start + 2 {
            x86_64::instructions::hlt();
        }
    }
}
```

Free all heap allocations and frames. Leaks fail with `the test leaked N heap bytes` or `the test leaked N frames`. Page tables are frames too: use `unmap_range`, or `prune_empty_tables` after raw `map_to`/`unmap`. See [leak checks](../architecture/testing.md#leaks).

## Integration test {#integration}

Use a separate kernel for custom initialization; `tests/heap_stress.rs` is an example. Provide `entry_point!` with `osel::BOOTLOADER_CONFIG`, call `osel::init`, `test_main()`, and use `osel::testosterone::test_panic_handler`. Ordinary `#[test_case]` integration files are discovered automatically. Only `harness = false` kernels need explicit Cargo registration. Without `osel::avganec::init`, screen printing is a no-op.

## Expected panic {#panic-test}

`osel::panic_test!` initializes the kernel and succeeds only if the body panics with all expected fragments; it also checks the red-screen header.

```rust
#![no_std]
#![no_main]

osel::panic_test!("overflow::has_a_handler", ["EXCEPTION: OVERFLOW"], {
    unsafe { core::arch::asm!("int 4") };
});
```

Register the file in `Cargo.toml`:

```toml
[[test]]
name = "overflow"
harness = false
```

### Check the fault address {#expect-rip}

Write the instruction address to `osel::testosterone::EXPECTED_RIP` immediately before the fault, as in `tests/invalid_opcode.rs`. `expect_panic` then requires `rip: <address>`:

```rust
osel::panic_test!("invalid_opcode::is_reported", ["EXCEPTION: INVALID OPCODE"], {
    unsafe {
        core::arch::asm!(
            "lea {rip}, [rip + 2f]",
            "mov [{expected}], {rip}",
            "2: ud2",
            rip = out(reg) _,
            expected = in(reg) osel::testosterone::EXPECTED_RIP.as_ptr(),
        )
    };
});
```

The `2:` label belongs to the faulting instruction; `lea` obtains its address.

## Run tests {#run}

| Task | Command |
|---|---|
| UEFI suite | `cargo test` |
| BIOS suite | `OSEL_BOOT=bios cargo test` |
| Continue after a failing kernel | `cargo test --no-fail-fast` |
| Kernel-module tests only | `cargo test --lib` |
| One integration kernel | `cargo test --test overflow` |
| Longer timeout | `OSEL_TIMEOUT=120 cargo test` |

Name filters after `--` are ignored; narrow the run with `--test` or `--lib`. See [runner](../reference/runner.md).

## Before a PR {#before-pr}

```shell
cargo test-core
cargo test-runner
cargo docs-check
cargo test --no-fail-fast
OSEL_BOOT=bios cargo test --no-fail-fast
OSEL_EXPECT="OSel ready to go!" cargo run
```

The last command boots the real kernel without a window and waits for readiness. CI repeats these checks.

## Common failures {#errors}

| Failure | Cause |
|---|---|
| `test`/`#[panic_handler]` compile error | Missing `harness = false` Cargo registration |
| Heap leak | Retained `Box`, static ownership or `mem::forget` |
| Frame leak | Unreleased data pages or page tables |
| `the test freed N frames it did not allocate` | Released pages allocated before this test |
| `N of M tests reported [ok]` | Early exit or mixed output; see [troubleshooting](troubleshooting.md#messages) |
| Only one firmware mode fails | Firmware initializes memory/devices differently; test both |
