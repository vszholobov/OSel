### how to run

Install [rustup](https://rustup.rs) and QEMU. The pinned nightly toolchain, its components and targets
are picked up automatically from `rust-toolchain.toml`.

```shell
brew install qemu                                   # macOS
sudo apt update && sudo apt install qemu-system-x86 # Linux
```

```shell
cargo run    # boot OSel in QEMU
cargo test   # kernel unit and integration tests, each test kernel boots in QEMU
```

The kernel is booted via UEFI by default. To boot via legacy BIOS:

```shell
OSEL_BOOT=bios cargo run
OSEL_BOOT=bios cargo test
```

Extra arguments after `--` go straight to QEMU, e.g. `cargo run -- -s -S` to wait for a debugger.

### tests

| command | what runs | needs QEMU |
|---|---|---|
| `cargo test-core` | unit tests of `osel-core`: console, allocators, memory map, keyboard decoding, exception reports | no |
| `cargo test-runner` | unit tests of the QEMU runner | no |
| `cargo test` | kernel tests (`#[test_case]` in `src/`) and integration tests in `tests/` | yes |
| `OSEL_EXPECT="OSel ready to go!" cargo run` | boots the real kernel headless and waits for the line on serial | yes |

Every test kernel gets 60 seconds, `OSEL_TIMEOUT=<seconds>` changes the limit. `cargo test -- <filter>`
is not supported: arguments are ignored for test kernels.

### how it works

The kernel is built for the `x86_64-unknown-none` target and uses
[bootloader 0.11](https://github.com/rust-osdev/bootloader). `osel-core/` holds the parts of the kernel
that don't touch hardware, so they build and are tested on the host as well. `runner/` is a small host
tool that cargo uses as the runner: it packs the kernel ELF into a UEFI or BIOS disk image
(`target/x86_64-unknown-none/debug/OSel.uefi.img` / `OSel.bios.img`) and starts QEMU.
UEFI firmware (OVMF) for QEMU is downloaded on the first run into `runner/.ovmf`
(`OSEL_OVMF_DIR` overrides the place).
