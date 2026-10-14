---
description: GDT/TSS, IDT, exceptions, panic screen, IST stacks, PIC and PIT timer.
---

# Interrupts and exceptions

**Audience:** developers changing handlers, GDT, timer or panic output.

**Summary:** all nonreserved processor exceptions have handlers. Kernel exceptions except breakpoint report state and panic. Double fault, NMI and machine check use guarded dedicated stacks. PIC delivers a 100 Hz timer and keyboard IRQs. See [vectors](../../reference/vectors.md).

## Behavior {#how}

### GDT and TSS {#gdt}

`src/gdt.rs` loads kernel code/data, ring-3 data/code ordered for sysret, and TSS. Reload CS, SS/DS/ES, clear FS/GS, load empty LDT and TSS with three IST stacks. Program switching changes RSP0; see [entry](user-mode.md#enter).

### Exceptions {#exceptions}

IDT in `src/interrupts.rs` routes user-capable exceptions through entry_stub. Ring 0 jumps to the original kernel handler; ring 3 follows [user traps](user-mode.md#traps). fatal_exception builds Frame (rip/rsp/cs/ss/rflags) and panics with ExceptionReport from `osel-core/src/exception.rs`.

Reports name the exception, decode selector errors (#TS/#NP/#SS/#GP) into table/index/external origin, and decode page-fault CR2, canonicality and access reason. Kernel int3 increments breakpoints and uses try_print without panicking.

### Panic screen {#panic-screen}

`src/main.rs` calls avganec::panic_screen. Disable interrupts and call enter_panic in `src/lib.rs`; nested panic emits one emergency_print line and returns false without drawing. Otherwise force-unlock WRITER, paint red with white OSel crashed :(, VERSION/COMMIT and panic text, then force-unlock SERIAL1 and duplicate it to serial. See [user crash reports](../../../user/crash-report.md).

### IST stacks {#ist}

`ist_stacks` in `src/gdt.rs` defines three static 32 KB stacks with bottom guard pages:

| Index | Constant | Purpose |
|---|---|---|
| 0 | DOUBLE_FAULT_IST_INDEX | Kernel stack may already overflow |
| 1 | NMI_IST_INDEX | Arrival at any time |
| 2 | MACHINE_CHECK_IST_INDEX | Arrival at any time |

memory::init unmaps guards so overflow faults instead of corrupting neighbors.

### PIC and timer {#pic-pit}

init_pics drains up to 64 keyboard-controller bytes, remaps PICs to PIC_1_OFFSET 32/PIC_2_OFFSET 40, programs PIT channel 0 to TICK_HZ 100 using pit::divisor, and unmasks only IRQ0/1 using pic_masks. keyboard::init explicitly establishes PS/2 readiness first, with interrupts disabled.

Timer increments ticks, sends EOI, wakes waiters and polls COM1/PS2. Two ticks of unread PS/2 data activate fallback polling. Then scheduler::tick may switch threads. Keyboard IRQ calls keyboard::interrupt and reads at most 16 ready bytes.

### Spurious IRQs {#spurious}

For IRQ7/15, read the PIC ISR and use pit::is_spurious. Missing ISR bit means spurious: IRQ7 gets no EOI; IRQ15 gets master-only EOI. Count with spurious_interrupts().

## Code {#code}

`src/gdt.rs`: init, selectors, ist_stacks, kernel_stack_slot, reload_data_segments. `src/interrupts.rs`: IDT, init_idt/init_pics, with_pics, entry stubs, fatal_exception, ticks. `osel-core/src/exception.rs`: ExceptionReport, SelectorError, PageFaultCause, FixedBuf. `osel-core/src/pit.rs`: divisor, masks and spurious checks. `src/drivers/avganec.rs`: panic_screen.

## Invariants {#invariants}

Hardware handlers do not allocate or print on screen. Only reserved vectors 9,15,22–27,31 lack handlers. Exception handlers cannot assume locks are free: exceptions occur even with interrupts masked.

## Locking {#locks}

Timer, keyboard and spurious handlers take PICS; keyboard additionally takes DECODER/BUFFER. IrqSpinlock prevents interrupted code on the same CPU from holding these locks when hardware IRQs arrive. See [locking](locking.md).

## Pitfalls {#pitfalls}

Reload bootloader SS into our data selector: a mismatching old selector caused iretq #GP and double fault. Load lldt 0 to remove reset-era LDT state that otherwise exposes address zero. Clear firmware FS/GS selectors before future reloads. Explicitly configure PIC/PIT rather than trusting UEFI masks/frequency.

Drain a boot-menu key byte before PIC reset or IRQ1 may stay asserted with no new edge. Spurious IRQ7/15 can arrive despite masks and need handlers. Print breakpoints with try_print because exceptions can interrupt a held screen lock. Do not put page faults on a shared IST stack: demand paging requires nesting. Use assembly div for division faults; Rust checks division by zero itself.

## Tests {#tests}

Kernel tests cover returning breakpoints, ticking timer, masks/spurious IRQs, RTC frequency, selectors/TR/LDT/IST and syscall ordering. Integration kernels include `tests/divide_error.rs`, `tests/invalid_opcode.rs`, `tests/general_protection.rs`, `tests/unhandled_vector.rs`, `tests/overflow.rs`, `tests/double_fault.rs`, `tests/stack_overflow.rs`, `tests/panic_screen.rs`, `tests/nested_panic.rs` and `tests/panic_while_printing.rs`.

## Decisions {#decisions}

[0001](../../decisions/0001-bootloader-0.11-and-uefi.md), [0003](../../decisions/0003-irq-spinlock.md).

_Checked against code: 2026-10-13, branch fix-ps2-keyboard._
