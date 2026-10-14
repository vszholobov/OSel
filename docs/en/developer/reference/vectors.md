---
description: OSel IDT vectors, exception names, IST stacks, error codes and hardware handlers.
---

# Interrupt vectors

**Audience:** developers reading exception reports or adding handlers.

**Summary:** `src/interrupts.rs` handles every nonreserved processor exception. Kernel exceptions except breakpoint panic through `fatal_exception`; user exceptions stop the program except IST vectors. Hardware interrupts start at 32. Missing handlers report SEGMENT NOT PRESENT with the vector encoded in the error. See [interrupts](../architecture/core/interrupts.md).

## Processor exceptions {#exceptions}

Fields are `InterruptDescriptorTable` names from `x86_64`. Selector errors identify a GDT, LDT or IDT entry.

| Vector | IDT field | Report name | IST | Error | Kernel behavior |
|---|---|---|---|---|---|
| 0 | `divide_error` | DIVIDE ERROR | — | None | Panic |
| 1 | `debug` | DEBUG | — | None | Panic |
| 2 | `non_maskable_interrupt` | NON-MASKABLE INTERRUPT | 1 | None | Panic on dedicated stack |
| 3 | `breakpoint` | — | — | None | Increment `breakpoints()`, `try_print`, continue |
| 4 | `overflow` | OVERFLOW | — | None | Panic |
| 5 | `bound_range_exceeded` | BOUND RANGE EXCEEDED | — | None | Panic |
| 6 | `invalid_opcode` | INVALID OPCODE | — | None | Panic |
| 7 | `device_not_available` | DEVICE NOT AVAILABLE | — | None | Panic |
| 8 | `double_fault` | DOUBLE FAULT | 0 | Number | Panic on dedicated stack |
| 10 | `invalid_tss` | INVALID TSS | — | Selector | Panic |
| 11 | `segment_not_present` | SEGMENT NOT PRESENT | — | Selector | Panic; also missing-handler result |
| 12 | `stack_segment_fault` | STACK SEGMENT FAULT | — | Selector | Panic |
| 13 | `general_protection_fault` | GENERAL PROTECTION FAULT | — | Selector | Panic |
| 14 | `page_fault` | PAGE FAULT | — | Page flags | Panic with CR2 and decoded reason |
| 16 | `x87_floating_point` | X87 FLOATING POINT | — | None | Panic |
| 17 | `alignment_check` | ALIGNMENT CHECK | — | Number | Panic |
| 18 | `machine_check` | MACHINE CHECK | 2 | None | Panic on dedicated stack |
| 19 | `simd_floating_point` | SIMD FLOATING POINT | — | None | Panic |
| 20 | `virtualization` | VIRTUALIZATION | — | None | Panic |
| 21 | `cp_protection_exception` | CONTROL PROTECTION | — | Number | Panic |
| 28 | `hv_injection_exception` | HYPERVISOR INJECTION EXCEPTION | — | None | Panic |
| 29 | `vmm_communication_exception` | VMM COMMUNICATION EXCEPTION | — | Number | Panic |
| 30 | `security_exception` | SECURITY EXCEPTION | — | Number | Panic |

Ring-3 exceptions terminate the program except IST vectors 2, 8 and 18. A first permitted fault on a reserved user page maps it and resumes execution; see [memory](../architecture/core/memory.md#areas). User #MF/#XM are ordinary computation faults. `int3` and any user `int N` cause #GP because all gates have DPL 0.

TrapFrame vector 256 denotes `syscall`, not an interrupt. Vectors 9, 15, 22–27 and 31 are reserved and have no handlers. If invoked, they produce SEGMENT NOT PRESENT. TSS IST indices in `src/gdt.rs` are 0 `DOUBLE_FAULT_IST_INDEX`, 1 `NMI_IST_INDEX`, 2 `MACHINE_CHECK_IST_INDEX`.

## Hardware interrupts {#hardware}

`InterruptIndex` uses `PIC_1_OFFSET` = 32 and `PIC_2_OFFSET` = 40.

| Vector | Index | IRQ | Handler | Behavior |
|---|---|---|---|---|
| 32 | `Timer` | 0 | `timer_interrupt_handler` | `ticks()` at 100 Hz, PIC EOI, `scheduler::tick` |
| 33 | `Keyboard` | 1 | `keyboard_interrupt_handler` | Read port 0x60 into keyboard buffer, EOI |
| 39 | `SpuriousMaster` | 7 | `spurious_master_handler` | If ISR bit absent, count `spurious_interrupts()` without EOI; otherwise EOI |
| 47 | `SpuriousSlave` | 15 | `spurious_slave_handler` | Spurious: count and master-only EOI; otherwise normal EOI |

Only IRQ 0/1 are unmasked (`ENABLED_IRQS`); spurious 7/15 can still arrive.

## Add a vector {#add}

Add the handler and IDT entry in `src/interrupts.rs`. Exceptions call `fatal_exception` with a name and `Details`; user-capable exceptions also need `entry_stub`, `through_stub` and `exception_name` in `osel-core/src/trap.rs`. Add a table row: `cargo docs-check` checks every IDT field and InterruptIndex variant in both languages. Add a `panic_test!` integration kernel following [testing](../guides/testing.md).

_Checked against code: 2026-10-13, commit 2c68d3f._
