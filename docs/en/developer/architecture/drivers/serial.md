---
description: COM1 output, presence detection, terminal byte transport and emergency panic output.
---

# Serial port

Language: [Русский](../../../../ru/developer/architecture/drivers/serial.md) · [English](serial.md)

**Audience:** developers changing serial output or test logs.

**Summary:** serial_print!/serial_println! write COM1, connected to the host terminal in QEMU. Probe once and silently skip output if absent. Panic has a lock-free emergency path.

## Behavior {#how}

port() in `src/cereal.rs` locks SERIAL1 and probes 0x3F8 with Uart16550Tty::new_port on first access. Failed probing leaves None. Macros call _print and ignore write errors. `src/main.rs` duplicates the version/commit banner to serial; release-image checks verify it before commands.

write_terminal sends raw bytes as FIFO becomes ready, without diagnostic UTF-8/CRLF rewriting. Release SERIAL1 before readiness waits; ordinary output is preemptible with interrupts enabled. Writes remain synchronous and repeatedly make nonblocking readiness checks.

receive reads only available bytes. Every 10 ms, tty::poll_serial reads up to 64 bytes into a preallocated buffer and wakes readers. IRQ4 remains masked: one-tick latency is enough for the game. This bounds input rate; overflow drops new bytes.

emergency_print writes registers directly without locks/probing; bounded readiness waits avoid hangs without a port. Nested panic and test_panic_handler use it.

## Code {#code}

`src/cereal.rs`: SERIAL1, port, _print, emergency_print and serial macros.

## Invariants {#invariants}

Missing COM1 is allowed. Panic does not depend on SERIAL1 ownership: the panic screen force-unlocks it and nested panic uses emergency output.

## Locking {#locks}

SERIAL1 is IrqSpinlock. Timer input neither waits nor allocates; IRQ handlers do not write serial output.

## Pitfalls {#pitfalls}

Do not initialize through lazy_static: panic during probing formerly left initialization stuck, including the panic path. PROBED now records the one attempt and failure is None. Most laptops have no COM1, making the screen essential.

## Tests {#tests}

`src/cereal.rs` checks QEMU presence and macro use in expressions. `tests/panic_while_printing.rs` checks a held SERIAL1; `tests/nested_panic.rs` checks emergency output.

_Checked against code: 2026-10-13, branch release-images._
