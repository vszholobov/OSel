### how to run

Install [rustup](https://rustup.rs) and QEMU. The nightly toolchain, its components and targets
are picked up automatically from `rust-toolchain.toml`.

```shell
brew install qemu                                   # macOS
sudo apt update && sudo apt install qemu-system-x86 # Linux
```

```shell
cargo run    # boot OSel in QEMU
cargo test   # run unit and integration tests in QEMU
```

The kernel is booted via UEFI by default. To boot via legacy BIOS:

```shell
OSEL_BOOT=bios cargo run
```

Extra arguments after `--` go straight to QEMU, e.g. `cargo run -- -s -S` to wait for a debugger.

### how it works

The kernel is built for the `x86_64-unknown-none` target and uses
[bootloader 0.11](https://github.com/rust-osdev/bootloader). `runner/` is a small host tool that
cargo uses as the runner: it packs the kernel ELF into a UEFI or BIOS disk image
(`target/x86_64-unknown-none/debug/OSel.uefi.img` / `OSel.bios.img`) and starts QEMU.
UEFI firmware (OVMF) for QEMU is downloaded on the first run.
