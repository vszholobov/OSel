---
description: PS/2 setup, scan-code decoding, bounded input, fallback polling and diagnostics.
---

# Keyboard

**Audience:** developers changing input.

**Summary:** configure i8042/PS2 explicitly, decode IRQ1 scan codes using US layout and queue 64 characters. Fallback polling handles missed IRQs. Boot diagnostics expose setup/counters; Ctrl+letter produces control bytes, such as Ctrl+C = \x03.

## Behavior {#how}

keyboard_interrupt_handler calls keyboard::interrupt, checking 0x64 before each 0x60 read and consuming at most 16 bytes. AUX, transmission errors and protocol responses do not enter the decoder. ScancodeDecoder in `osel-core/src/keyboard.rs` handles set 1, Shift, Caps Lock and E0. Releases, modifiers, nonsymbol keys and garbage return None. RingBuffer holds BUFFER_SIZE 64; overflow drops new characters and increments dropped_chars. tty::notify_input wakes readers without switching or allocating in IRQ. pop_char retrieves oldest input. The [shell](../shell.md) reads Tty except while a program owns input.

### Initialization {#init}

Before PIC setup and enabling interrupts, `osel-core/src/ps2.rs` uses Io (ports in kernel, model in host tests) to:

1. Disable both ports (AD/A7), await commands and drain old data.
2. Read command byte (20), record it, mask IRQ1/12, disable AUX and enable set-2-to-set-1 translation while preserving other bits.
3. Enable keyboard port AE, stop scanning F5, select set 2 with F0/02. Require FA ACK per byte; FE retries only that byte, at most three attempts.
4. Enable IRQ1, keep AUX/IRQ12 disabled, read back and verify configuration before scanning.
5. Enable scanning F4 and require ACK. Only Ready allows decoding; failed setup still counts raw bytes without creating commands.

Waits have 10000 checks with I/O delay (32 reads of 0x80), independent of PIT: an operation budget, not a precise duration. Drain is limited to 64 bytes; ACK waits admit 16 unrelated responses. Missing controller status FF, timeout, bad response and configuration mismatch are recorded without preventing boot. No controller/keyboard self-test/reset or CPU/A20 reset-port writes. Host models check the command/ACK sequence.

### Fallback polling {#poll}

Every 10 ms keyboard::poll checks for unread data. Two consecutive ticks with a full output buffer activate the same decoding path. This compensates for missed IRQ1, not missing APIC/ACPI/PIT setup. Poll and IRQ do not duplicate data because status/read run interrupt-masked on one CPU.

## Diagnostics {#diagnostics}

Before the first prompt, PS/2 init shows error, command bytes before/after, last response, retries and drained bytes. Ready/error=None proves protocol success, not IRQ delivery. KBD refreshes once a second until the first PS2/COM1 input byte, preserving the prompt cursor; later kbd prints a snapshot.

| Field | Meaning |
|---|---|
| t | PIT ticks; changing value proves timer/shell progress |
| irq | IRQ1 handler calls, including empty calls |
| bytes | Runtime reads, excluding initialization |
| poll | Bytes consumed by fallback |
| chars | Decoded characters including control bytes, not physical keypresses |
| bad | Filtered AUX/errors/protocol bytes or unready setup |
| st | Current controller status in hex |
| last | Last runtime byte in hex, initially 00 |

Growing t but bytes=0 points to controller/scanning. Growing poll/chars with irq=0 means input recovered but IRQ delivery is faulty. Growing bytes without chars requires inspecting bad/scan codes/setup. No refresh before input suggests PIT/interrupt failure or a hang; a short video is more useful than one photo.

## Code {#code}

`src/drivers/keyboard.rs`: init, interrupt, poll, diagnostics, write_diagnostics, DECODER, BUFFER, handle_scancode, pop_char, dropped_chars. `osel-core/src/ps2.rs`: protocol and filtering. `osel-core/src/keyboard.rs`: ScancodeDecoder/RingBuffer. `src/interrupts.rs`: handler and controller drain.

## Invariants {#invariants}

IRQ only decodes/enqueues: no allocation or printing. Tty checks and blocks atomically with interrupts off to avoid lost wakeups. Protocol exchanges occur only during masked initialization; IRQ/timer never wait for a response.

## Locking {#locks}

DECODER precedes BUFFER. KEYBOARD_DIAGNOSTICS is released before decoding, waking Tty or printing; character accounting may acquire it under DECODER, never in reverse.

## Pitfalls {#pitfalls}

Drain stuck boot-menu bytes before PIC reset. UEFI-menu keyboard input does not establish post-handoff PS2 readiness. The old image reached the shell without input on Lenovo Xiaoxin Pro 14/Ryzen AI 7 H 350; Linux identified AT Translated Set 2 keyboard. The fixed image still needs hardware retesting. Runtime AA is left-Shift release in set 1, not a self-test response. USB keyboards need firmware PS2 emulation; OSel has no USB driver. Only US layout is implemented; no Russian input.

## Tests {#tests}

Host tests cover buffer sizes 0/1, order/overflow, modifiers, control/navigation keys, Pause/Print Screen and garbage. Kernel tests inject via controller command D2, check Shift/extended keys/overflow, reinitialize disabled settings, poll masked IRQ without duplicates, and filter responses. PS2 model tests cover ordering, system-bit preservation, ACK/RESEND, missing/busy controllers, stuck data, no response, errors and readback. `tools/check-shell.py` checks automatic diagnostics, kbd and actual QEMU sendkey; --boot-screenshot captures boot diagnostics.

_Checked against code: 2026-10-13, branch fix-ps2-keyboard._
