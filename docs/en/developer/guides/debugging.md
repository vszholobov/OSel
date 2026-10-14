---
description: Debug OSel with serial output, panic addresses, LLDB/GDB, QEMU exception logs and screenshots.
---

# Debugging

**Audience:** developers investigating failures.

**Summary:** start with the panic text on the red screen and serial output. Resolve RIP using LLDB. For stepping, run `cargo run -- -s -S` and attach a debugger to port 1234. For resets without a report, use `-d int,cpu_reset`.

Arguments after `cargo run --` go directly to QEMU. Test kernels discard additional arguments.

## Serial output {#serial}

`serial_print!` and `serial_println!` in `src/cereal.rs` use COM1. QEMU connects it to stdout; runner filters firmware chatter in noninteractive modes. Panic text is duplicated as `OSel <version> (commit <hash>) crashed: …`. Printing works before framebuffer initialization. Missing COM1 is silently tolerated.

## Analyze a panic screen {#panic-screen}

See [screen fields](../../user/crash-report.md#anatomy). `panicked at` identifies the panic call; processor exceptions usually point to a handler in `src/interrupts.rs`, so inspect RIP instead. The relocatable kernel ELF starts at zero and currently loads at `0xffff800000000000`. Subtract that address from RIP:

```shell
lldb target/x86_64-unknown-none/debug/OSel
(lldb) image lookup --address 0x18c11
```

For tests, use the ELF path printed by Cargo's `Running tests/<name>.rs (…)` line. Page faults also report `accessed address` and `reason`; see [memory](../architecture/core/memory.md).

If lookup fails, compare `_start` from `llvm-nm target/x86_64-unknown-none/debug/OSel` with the debugger's actual entry address; do not assume the load offset is permanent.

## Step through execution {#debugger}

Start paused with the GDB server:

```shell
cargo run -- -s -S
```

Attach in another terminal with relocated symbols:

{% list tabs %}

- LLDB

  ```shell
  lldb target/x86_64-unknown-none/debug/OSel
  (lldb) target modules load --file OSel --slide 0xffff800000000000
  (lldb) gdb-remote 1234
  (lldb) breakpoint set --name kernel_main
  (lldb) continue
  ```

- GDB

  On macOS, an x86_64 debugger can be installed with `brew install x86_64-elf-gdb`.

  ```shell
  x86_64-elf-gdb
  (gdb) add-symbol-file target/x86_64-unknown-none/debug/OSel -o 0xffff800000000000
  (gdb) target remote :1234
  (gdb) hbreak kernel_main
  (gdb) continue
  ```

{% endlist %}

Use hardware breakpoints (`hbreak` or `breakpoint set --hardware`) if ordinary ones do not trigger. Software kernel breakpoints do not work before 64-bit mode.

## Reset without a panic screen {#triple-fault}

A triple fault resets the CPU. With runner's `-no-reboot`, QEMU exits instead. Log preceding exceptions:

```shell
cargo run -- -d int,cpu_reset -D /tmp/qemu.log
```

Close QEMU if the kernel does not fail. For a bounded run without a window:

```shell
OSEL_EXPECT="OSel ready to go!" cargo run -- -display none -d int,cpu_reset -D /tmp/qemu.log
```

Inspect entries before `CPU Reset`: `v=0e` is page fault, `v=08` is double fault. The log includes error codes and registers. See [vectors](../reference/vectors.md).

## Screenshot {#screenshot}

```shell
cargo run -- -display none -monitor unix:/tmp/osel-mon.sock,server,nowait
```

In another terminal:

```shell
echo "screendump /tmp/osel.png -f png" | nc -U /tmp/osel-mon.sock
echo "quit" | nc -U /tmp/osel-mon.sock
```

The monitor also accepts `sendkey a` and `sendkey shift-1`. Test kernels normally report through serial and exit themselves. To inspect one visually, rebuild its deleted test image and launch it directly with `qemu-system-x86_64 -drive format=raw,file=<image> …`.

## Common errors {#errors}

| Symptom | Action |
|---|---|
| Breakpoint does not trigger | Check symbol slide and use hardware breakpoints |
| Address lookup fails | Subtract the load offset and use the matching ELF |
| QEMU exits without output | Inspect [triple-fault logs](#triple-fault) |

See [troubleshooting](troubleshooting.md) for known symptoms.
