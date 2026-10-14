---
description: Named spinlocks, interrupt masking, kernel lock inventory and acquisition rules.
---

# Locking

**Audience:** developers adding globals or calling code from interrupt handlers.

**Summary:** kernel globals use IrqSpinlock. It masks interrupts until release and restores the previous state. After 50 million failed attempts it panics with the lock name and owner/wait locations. Add every new named lock to both language inventories.

## Behavior {#how}

### Spinlock {#spinlock}

Spinlock in `osel-core/src/sync.rs` tries lock up to DEFAULT_MAX_SPINS, recording caller location with track_caller. try_lock never waits. force_unlock is reserved for panic paths. This layer does not change interrupt flags and supports host tests.

### IrqSpinlock {#irq-spinlock}

`src/sync.rs` saves IF, disables interrupts, then acquires the inner lock. Release unlocks before restoring IF. Nested locks only restore interrupts when the outer guard drops. Failed try_lock preserves the original state. Hardware IRQs cannot interrupt a lock held by their CPU; processor exceptions and recursive acquisition can still deadlock and produce reports.

## Kernel lock inventory {#locks}

| Lock | Location | Protected state and callers |
|---|---|---|
| `WRITER` | `src/avganec.rs` | Framebuffer; print!/with_console/read_pixel, breakpoint try_lock, panic force-unlock |
| `SERIAL1` | `src/cereal.rs` | COM1; port/serial macros, panic force-unlock |
| `PICS` | `src/interrupts.rs` | PIC pair; setup, timer/keyboard/spurious handlers |
| `DECODER` | `src/keyboard.rs` | Scan-code state, keyboard input |
| `BUFFER` | `src/keyboard.rs` | Character queue, IRQ/pop_char/dropped_chars |
| `KEYBOARD_DIAGNOSTICS` | `src/keyboard.rs` | Setup report and runtime counters, IRQ/poll/snapshot |
| `ALLOCATOR` | `src/allocator.rs` | Heap, global allocator/with_allocator |
| `MEMORY` | `src/memory.rs` | Tables/frame pool, with_memory and user demand faults |
| `TTY_TEXT` | `src/tty.rs` | Partial UTF-8 output, Tty::write; release before printing |
| `TTY_INPUT` | `src/tty.rs` | termios/input, read/get_termios/set_termios |
| `TTY_SERIAL_INPUT` | `src/tty.rs` | Queued COM1 bytes, poll_serial/read |
| `TTY_WAITERS` | `src/tty.rs` | Input waiters, read/notify_input/forget; capacity allocated at boot |
| `TTY_OUTPUT` | `src/tty.rs` | Writer ownership/waiters, OutputGuard/forget; sleep without spinlock |
| `OFFSET` | `osel-core/src/file.rs` | Shared regular-file offset, plain Spinlock around seekable read/write |
| `SCHEDULER` | `src/scheduler.rs` | Threads/current/ready queue, schedule/spawn/wake/block/exit/tick/finish_switch; release before switch_to |
| `STACK_SLOTS` | `src/scheduler.rs` | Kernel stack slots, allocation/release |
| `MM` | `src/scheduler.rs` | Per-process space, short nonblocking space()/fault operations |
| `FILES` | `src/scheduler.rs` | Per-process descriptors, release before file read/write |
| `PROCESS_EXIT` | `src/scheduler.rs` | Process result, exit_current/wait |
| `PROCESS_WAITER` | `src/scheduler.rs` | Kernel process waiter, wait/exit_current |
| `CHILDREN` | `src/scheduler.rs` | Children/unconsumed results, vfork/wait_child/parent exit; release before waiting |
| `CHILD_WAITERS` | `src/scheduler.rs` | Parent threads waiting for children; waking takes SCHEDULER |
| `VFORK` | `src/scheduler.rs` | Launch wait, release_vfork; unlock before waking |
| `SLEEPERS` | `src/time.rs` | Deadlines, sleep_until/wait_until/wake_due |
| `REALTIME_BASE` | `src/time.rs` | Unix base time, init/realtime_ns |
| `FUTEXES` | `src/futex.rs` | Address waiters, futex/wake |
| `POLL_WAITERS` | `src/poll.rs` | epoll_pwait sleepers, epoll_pwait/notify |
| `PIPE` | `src/pipe.rs` | Pipe data/endpoints, read/write/close; unlock before sleeping |
| `PIPE_WAITERS` | `src/pipe.rs` | Data/space waiters and teardown; waking takes SCHEDULER |
| `EVENTFD`, `EPOLL` | `osel-core/src/poll.rs` | Counter/watch list, plain Spinlock |
| `ROOT_FS` | `src/fs.rs` | Root filesystem, init/root; hold only to clone Arc |

Existing nesting: DECODER → BUFFER or KEYBOARD_DIAGNOSTICS; SCHEDULER → ALLOCATOR when the thread table changes; MM → MEMORY when mapping pages, including user faults. Keep other locks separate.

## Code {#code}

Keyboard diagnostic ownership is released before decoding/waking; character accounting may take it under DECODER, never the reverse. Take a snapshot before console locking. `osel-core/src/sync.rs` owns Spinlock/SpinlockGuard/DEFAULT_MAX_SPINS; `src/sync.rs` owns IrqSpinlock/IrqSpinlockGuard.

## Rules {#rules}

Protect mutable globals with named IrqSpinlock::new using an uppercase static name. Add its row for docs-check. Keep guards short and never reenter the same lock through print, allocation or with_memory. Exception handlers use try_lock only. force_unlock is for nonreturning panic paths, never ordinary recovery.

## Pitfalls {#pitfalls}

Processor exceptions ignore IF; masked interrupts do not prevent an int3/page fault from reentering a held lock. Breakpoint output uses try_print. The 50-million-spin report may take seconds in software QEMU. Future heap growth would take MEMORY under ALLOCATOR, so never allocate inside with_memory. On multiple CPUs ordinary long contention can exceed the budget, and panic force_unlock needs redesign.

## Tests {#tests}

Core tests verify owner/wait reports, try_lock/force_unlock and invariants under eight contending threads. Kernel sync tests check IF for acquisition/nesting/failed try_lock and continuing ticks. Memory/allocator/console tests check IF under guards. `tests/deadlock.rs`, `tests/memory_reentrancy.rs` and `tests/panic_while_printing.rs` cover reentry and panic with held WRITER/SERIAL1.

## Decisions {#decisions}

[0003](../../decisions/0003-irq-spinlock.md).

_Checked against code: 2026-10-13, branch fix-ps2-keyboard._
