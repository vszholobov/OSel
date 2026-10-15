---
description: OSel shell commands, bounded input, ramdisk program launch and return after gameplay.
---

# Shell

**Audience:** developers changing command input or program launch.

**Summary:** the kernel shows `osel>` after boot. Try `help`, `ls /bin`, `cat /etc/motd` or `tetris`. This minimal kernel shell runs one foreground program and uses a read-only filesystem.

## Commands {#commands}

| Command | Action |
|---|---|
| `diag`, `pci`, `net`, `net ip/route/dns/tcp/dhcp`, `ping`, `nslookup`, `tcpcheck`,  `wifi status/fw/regs/probe` | [Diagnostics](../../user/diagnostics.md#commands) |
| `help` | Commands and limitations |
| `kbd` | PS/2 initialization and input counters; [diagnostics](drivers/keyboard.md#diagnostics) |
| `ls [path]` | Directory contents, default root `/` |
| `cat path` | Regular text file up to 64 KiB; replace control characters other than newline/tab with `?` |
| `tetris` | `/bin/tetris --single`; x returns |
| `clear` | External `/bin/clear` |
| `hello`, `gohello` | Examples; gohello exits with 30, shown by shell |
| `/bin/name arguments` | Run an executable |

Names without `/` are found in `/bin`; relative paths start at root. Tetris receives `--single` automatically unless already present, so `tetris --seed 2` remains single-player.

## Input {#input}

PS/2 and COM1 feed shared `/dev/tty`. Enter submits; Backspace deletes; Ctrl+C/Ctrl+U cancel the line. Serial CR/LF is one Enter. Empty lines do nothing. Up/Down recall commands; Enter executes the selection, and editing leaves the saved entry intact. Down past the newest entry restores the draft. Both PS/2 and COM1 CSI A/B or SS3 A/B work. PageUp/PageDown and Ctrl+Shift+Up/Down control [scrollback](drivers/console.md#scrolling).

The editor accepts printable ASCII and tabs as spaces. Length is at most 256 bytes and one display line minus prompt and one spare cell, keeping Backspace within the line. Overflow or non-ASCII rejects the entire command. The parser accepts at most 16 arguments. Quotes, escapes, variables, pipes, redirection, separators and background commands are rejected explicitly.

## Behavior {#how}

`src/main.rs` invokes `shell::run` in the main kernel thread after readiness. Before the first input byte, nonblocking reads alternate with 10 ms sleep and keyboard diagnostics update once a second, preserving cursor position. First input from either source stops updates and switches Tty reads to blocking, except while awaiting an Escape continuation. Ten ticks without another byte treat a lone Escape as return-to-live, then blocking reads resume. IRQ handlers neither parse nor print commands.

LineEditor reserves a 64-entry ring of at most MAX_LINE bytes per entry at construction. Nonempty submitted lines are retained; consecutive duplicates, canceled input and overflow/non-ASCII rejections are skipped. The draft retains text and rejection state, so browsing cannot make a truncated draft executable. Key ANSI parameters are bounded to 16 bytes; unknown sequences are discarded. Recall redraws one prompt line using CR CSI 2K.

Built-ins cover files, keyboard and [network diagnostics](../../user/diagnostics.md). `ls` uses `Inode::read_dir`; cat checks type and size before reading. `write_file_text` in `osel-core/src/shell.rs` filters terminal controls and emits up to 512 bytes per write without splitting UTF-8; a missing final newline is appended. Other commands verify executable regular files and call `program::run`, passing argv and `PATH=/bin`, `TERM=xterm`, `GODEBUG=asyncpreemptoff=1`.

For Tetris, the shell sends CSI ?1049h before launch and CSI ?1049l on every return from program::run. This [alternate screen](drivers/console.md#scrolling) preserves shell contents and excludes game frames from scrollback.

On exit, load error or program fault, restore terminal settings, discard residual input and reset color/cursor-visibility state. Report errors/nonzero status before the next prompt. [Process teardown](core/threads.md#process) handles descendants and resources.

## Code {#code}

`osel-core/src/shell.rs` owns `LineEditor`, parsing and executable paths. `src/shell.rs` owns the loop, built-ins, launch and restoration; `src/main.rs` enters it. `tests/shell.rs` checks VFS and repeated execution; `tools/check-shell.py` verifies the real kernel through COM1/PS2 in both boot modes.

## Limitations {#pitfalls}

No separate `/bin/sh`, scripts, cd, left/right cursor editing, background jobs or process-group control. Ctrl+C cancels a shell line but delivers no signal to programs; Tetris handles its byte itself. All programs share one terminal. Ramdisk is read-only; cat excludes devices and large binaries. Clear pending game input on return. Without Go, Tetris and Go examples are absent while other commands work.

## Tests {#tests}

`cargo test-core` checks the history ring, editing recalled commands, restoring drafts/rejection flags, CR/LF, cancellation, overflow, ANSI keys and parser limits. `cargo test --test shell` checks commands, VFS failures, repeated hello/clear and Go status, with leak checks.

`OSEL_BOOT=uefi python3 tools/check-shell.py` (or bios) boots the real headless kernel and checks pre-input diagnostics, kbd/help/ls/cat, unknown commands, Backspace, rejected pipes/long lines, clear, gohello, two Tetris runs and PS/2 help with IRQ/event counters. It also checks COM1/PS2 command recall, draft restoration, PageUp/PageDown, Ctrl+Shift+Up/Down, Escape/input returning to live and Tetris alternate-screen entry/exit. Framebuffer snapshots must differ in history and match byte-for-byte after returning. Mismatch stops QEMU and fails. `--screenshot /tmp/osel-shell.png` saves the final screen; `--boot-screenshot /tmp/osel-keyboard.png` saves initial diagnostics.

_Checked against code: 2026-10-15, fix-console-scroll branch._

The `net` command shows Ethernet and DHCP status. `tetris --network --server IP:port` enables [network play](../../user/network.md); without `--network`, the shell still inserts `--single`.
