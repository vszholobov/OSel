---
description: Install development tools, run OSel in QEMU and test it on macOS or Linux.
---

# Build and run

Language: [Русский](../../../ru/developer/guides/quickstart.md) · [English](quickstart.md)

**Audience:** developers building OSel from source.

**Summary:** install rustup and QEMU, clone the repository and run `cargo run`. Cargo obtains the pinned Rust toolchain and builds the boot image.

## Requirements {#requirements}

You need git, a linker and build tools, rustup, QEMU, and Go 1.27 for Tetris and Go tests. Without Go, the kernel and assembly programs in Rust crates still build.

{% list tabs %}

- macOS

  Install Apple tools and Homebrew if absent, then rustup and QEMU:

  ```shell
  xcode-select --install
  /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  brew install qemu
  ```

- Linux (Debian, Ubuntu)

  ```shell
  sudo apt update && sudo apt install build-essential git curl qemu-system-x86
  curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  ```

{% endlist %}

Accept rustup defaults. Open a new terminal or run `. "$HOME/.cargo/env"` afterward. `rust-toolchain.toml` selects the compiler, components and target automatically.

## Run {#run}

```shell
git clone --recurse-submodules https://github.com/vszholobov/OSel
cd OSel
cargo run
```

The first run downloads Rust and OVMF and builds the bootloader; later runs are faster. An existing clone needs `git submodule update --init --recursive`.

Expect `OSel 0.1.0 (commit …) woke up!`, `Hello from ring 3!`, `OSel ready to go!` and `osel>`. Try `help`, `ls /bin`, `cat /etc/motd` or `tetris`. Game controls are `a`/`d`, `s`, `q`/`e`, with `x` to exit. Tetris requires Go at build time. See [shell](../architecture/shell.md).

Close QEMU or press Ctrl+C in the launch terminal to exit. UEFI is the default; to use BIOS:

```shell
OSEL_BOOT=bios cargo run
```

## Tests {#tests}

| Command | Coverage | QEMU |
|---|---|---|
| `cargo test-core` | Hardware-independent kernel logic | No |
| `cargo test-runner` | QEMU runner | No |
| `cargo test` | All kernel test images in UEFI | Yes |
| `OSEL_BOOT=bios cargo test` | Same tests in BIOS | Yes |

See [adding tests](testing.md).

## Disk images {#images}

Images are built before QEMU starts: `target/x86_64-unknown-none/debug/OSel.uefi.img` and, after a BIOS run, `target/x86_64-unknown-none/debug/OSel.bios.img`. They can be used for [USB boot](../../user/usb.md).

## Common errors {#errors}

| Error | Action |
|---|---|
| `qemu-system-x86_64: command not found` | Install QEMU |
| `cargo: command not found` after rustup | Open a new terminal or source Cargo's environment |
| `linker 'cc' not found` | Install Apple developer tools or Linux `build-essential` |
| OVMF download fails | Internet is needed once; retry, using the `runner/.ovmf` cache |
| Red crash screen | Save a [crash report](../../user/crash-report.md) |

See [troubleshooting](troubleshooting.md) for other symptoms.
