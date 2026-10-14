---
description: Runner environment variables, boot modes, image paths, exit codes and test success rules.
---

# Runner

**Audience:** anyone launching the kernel or tests.

**Summary:** `runner/` runs on the host, builds a disk image from the kernel ELF and launches QEMU. Cargo invokes it instead of executing the kernel directly. Configure it with `OSEL_*`; see [testing](../architecture/testing.md).

## Invocation {#invocation}

`.cargo/config.toml` selects `runner/run.sh` for `x86_64-unknown-none`. The script uses `cargo run --locked --target <host-tuple>` to build/run the host runner, passing the kernel ELF and QEMU arguments. Cargo resolves the script from the repository root, so commands also work from subfolders.

## Environment variables {#env}

| Variable | Values | Default | Behavior |
|---|---|---|---|
| `OSEL_BOOT` | `uefi`, `bios` | `uefi` | Build/select firmware image; empty means unset; other values fail |
| `OSEL_EXPECT` | Text | Unset | Boot real kernel headlessly and succeed when serial output contains it; empty means unset |
| `OSEL_TIMEOUT` | Positive integer seconds | 60 | Timeout for Test/Expect |
| `OSEL_OVMF_DIR` | Directory | `.ovmf` inside runner | UEFI firmware cache |
| `OSEL_RAMDISK` | Directory | Repository `ramdisk` | Archive source; relative to repository root, not cwd; empty means unset |

Invalid boot/timeout values fail before QEMU with `unknown OSEL_BOOT value …` or `OSEL_TIMEOUT must be a positive number of seconds, got …`. Missing ramdisk directories, non-file/non-directory entries and failed program builds also stop startup. Only regular files and directories can be packed.

## Programs from user/ {#user-programs}

Build `USER_PROGRAMS` before packaging: `cargo build --locked --release --target x86_64-unknown-none` for `user/<name>/Cargo.toml`, in separate `user/target`, with `-Crelocation-model=static` because only `ET_EXEC` is loadable. Do not propagate outer `RUSTFLAGS`, `CARGO_TARGET_DIR` or `CARGO_BUILD_TARGET`. Add executables as `/bin/<name>`, mode `0755`, after ramdisk files. Duplicate `bin/<name>` in the directory and program list is an error; build failures also abort.

The list includes `hello` and `clear`. `user/clear/src/main.rs` emits ANSI clear/home through `write` and exits. Tetris invokes `exec.Command("clear")` with `PATH=/bin`.

## Programs from user-go/ {#go-programs}

Build `GO_PROGRAMS` with host Go: `go build -mod=readonly -trimpath` in `user-go/<name>/`, setting `CGO_ENABLED=0`, `GOOS=linux`, `GOARCH=amd64`, `GOAMD64=v1`, `GOTOOLCHAIN=local`. This creates static baseline x86_64 binaries without fetching a toolchain. Add them to `/bin`.

Without Go, omit binaries and skip dependent tests; actual build failures abort. CI installs Go. `gohello` checks goroutines/timers, `goexec` checks children/pipes through `os/exec`; tests are `tests/go_hello.rs` and `tests/go_exec.rs`.

## Actual Tetris {#tetris}

After Go test programs, build `apps/tetris/client` from the pinned submodule with the same settings and package `/bin/tetris`; output is `user-go/target/tetris`. Go's normal module cache obtains dependencies. `-mod=readonly` forbids dependency-file edits, and release CI checks module files and gitlinks stayed unchanged. Missing Go permits a skip; missing submodule with installed Go is an error requiring `git submodule update --init --recursive`.

For `tetris-*` test kernels, QEMU stdin uses a runner pipe. `TetrisInput` waits for traced confirmations and sends real key bytes. It requires the whole [scenario](../architecture/tetris.md#tests), in addition to normal success rules.

## Modes {#modes}

| Mode | Selection | QEMU behavior | Result |
|---|---|---|---|
| Run | Ordinary cargo run | Window, interactive input | QEMU exit code or 128 + signal |
| Test | ELF name ends in a 16-hex-digit hash | `isa-debug-exit` at `0xf4`, headless, 128 MB, no terminal stdin | Apply success rules; delete disk image |
| Expect | Not a test and OSEL_EXPECT set | Headless, no terminal stdin | Stop QEMU when text is found |

All modes use `-serial stdio -no-reboot`. Standard VGA EDID requests 1280×1024 via `-vga std -global VGA.xres=1280 -global VGA.yres=1024`; resizing the window scales the image without changing terminal geometry.

## Arguments after -- {#test-args}

`cargo run -- -s -S` forwards arguments to QEMU. Test kernels warn and discard them, so `cargo test -- <filter>` cannot filter tests. Use `cargo test --test <name>` or `cargo test --lib`.

## Kernel exit codes {#exit-codes}

| Kernel action | Port 0xf4 | QEMU code |
|---|---|---|
| `exit_qemu(QemuExitCode::Success)` | `0x10` | 33 |
| `exit_qemu(QemuExitCode::Failed)` | `0x11` | 35 |
| Reset with -no-reboot | — | 0 |

## Test success rules {#pass}

QEMU must exit before timeout with code 33. If `Running N tests` appears anywhere in a line (firmware may omit a newline), exactly N lines must end with `[ok]`. Without a count, harness-free tests need at least one `[ok]`. Text containing `[ok]` elsewhere is not counted. Errors report timeout, exit code, count mismatch or `failed: no test reported [ok]`.

## Expect completion {#expect}

Found text means success and QEMU termination. Missing text at timeout produces `failed: … not printed within 60s`; premature exit produces `failed: kernel stopped with … before printing …`.

Test/Expect strip ANSI and OVMF `BdsDxe:` lines while preserving kernel blank lines. Run forwards sequences for the interactive game. Every received chunk is flushed, including partial frames without a newline.

## Files {#files}

| Item | Location |
|---|---|
| Images | Beside ELF: `<name>.uefi.img`/`<name>.bios.img`; normal debug kernel in target/x86_64-unknown-none/debug |
| OVMF | `runner/.ovmf`, fetched once through ovmf-prebuilt, excluded from Git |
| Ramdisk | Built in memory from `ramdisk/` or OSEL_RAMDISK, placed in the image as ramdisk; no separate host archive |
| Bootloader logging | Warn; only problems are printed |

## Code {#code}

`runner/run.sh` is the Cargo wrapper. `runner/src/main.rs` contains `parse_boot`, `parse_timeout`, `ramdisk_dir`, `pack_ramdisk`, `is_test_binary`, `image_path`, `create_disk_image`, `ovmf_firmware`, `forward_serial_output`, `test_outcome`, `expect_outcome` and `wait_with_timeout`.

_Checked against code: 2026-10-13, branch release-images._
