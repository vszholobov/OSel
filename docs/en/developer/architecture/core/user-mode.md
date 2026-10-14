---
description: Ring-3 segments, TrapFrame, exception/syscall entry, FPU state and validated program-memory copying.
---

# User mode

**Audience:** developers changing kernel/program transitions.

**Summary:** each program thread enters ring 3 using iretq from TrapFrame at its kernel-stack top. Exceptions preserve all registers and terminate the program; system calls use the same frame and run Rust dispatch with interrupts enabled. user::run launches and waits. See [threads](threads.md).

## Behavior {#how}

### Segments {#segments}

`src/gdt.rs` orders kernel code 0x08, kernel data 0x10, user data 0x18, user code 0x20 and TSS. RPL-3 user selectors are 0x1b/0x23. sysret derives data/code from one base at +8/+16, requiring this order. Mutable TSS RSP0 is set through kernel_stack_slot. Packed TSS makes RSP0 unaligned; use assembly or write_unaligned.

### TrapFrame {#trap-frame}

`osel-core/src/trap.rs` defines 22 words: 15 general registers r15 through rax, vector, error code (zero if absent), then rip/cs/rflags/rsp/ss. Tests verify offsets used by assembly. TrapFrame::new_user zeros registers, including System V RDX, and enables interrupts to avoid exposing kernel state.

### Enter a program {#enter}

user::run requires enabled interrupts/no IrqSpinlock. make_user forces ring-3 selectors and allowed RFLAGS with interrupts enabled; invalid high RIP becomes a user #GP without entering, since iretq would fault in the kernel. user::start transfers space/descriptors into a Process and creates a thread with a top-of-stack frame. First switching uses return_to_user; the switch installs CR3, RSP0 and FPU state. Process::wait returns the space, files, last frame and FPU state.

### Ring-3 exceptions {#traps}

`entry_stub` in `src/interrupts.rs` covers exceptions except double fault/NMI/machine check on IST. Saved CS selects the origin: ring 0 jumps untouched to the original handler; ring 3 adds missing error-code zero and vector, then user_trap saves 15 registers, clears DF and calls user_fault.

user_fault reads CR2 before enabling interrupts. A missing lower-half page first goes to map_on_demand: if its [area](memory.md#areas) permits access, map it and resume the same instruction. Otherwise stop with Stop::Fault. set_handler_fn checks handler type before replacing the address; macros check whether the vector has an error code.

Hardware IRQs use RSP0 and compiler x86-interrupt entry/iretq. Timer may switch threads. Privileged instructions, I/O and int N cause #GP (IDT gates have DPL 0); kernel-page access causes #PF.

### System calls {#syscall}

`src/syscall.rs` enables EFER.SCE and sets STAR selectors, LSTAR syscall_entry and SFMASK clearing IF/DF/TF/AC/NT/IOPL.

Entry saves user RSP and switches to CURRENT_STACK_TOP before enabling interrupts. It builds TrapFrame vector SYSCALL_VECTOR = 256 using RCX return RIP and R11 flags, then sti calls dispatch. Dispatch writes result or negative errno to RAX and return_path selects sysretq or iretq with TF. Cancellation/invalid return addresses terminate through exit_current. Return disables interrupts and restores registers; sysret RCX/R11 come from frame RIP/RFLAGS.

An address outside the lower half must be rejected before either return instruction, which could otherwise fault in ring 0. exit calls exit_thread; exit_group calls exit_current. See [call reference](../../reference/syscalls.md) and [processes](threads.md#process).

### x87 and SSE {#fpu}

`src/fpu.rs` clears CR0 EM/TS, sets MP/NE, and sets CR4 OSFXSR/OSXMMEXCPT. XSAVE/AVX are disabled. BIOS does not establish these settings; UEFI often enables SSE itself.

`FpuState` in `osel-core/src/fpu.rs` stores 512-byte fxsave64 state. Switching saves/restores it and final process results retain it. run_with accepts/returns custom state; run uses FCW 0x037F, MXCSR 0x1F80, empty x87 stack and zero XMM. Kernel SSE is disabled so traps/calls do not overwrite user registers. See [0013](../../decisions/0013-fpu-state.md) and [0015](../../decisions/0015-threads.md).

### user/ programs {#user-programs}

Independent no_std crates in `user/<name>/` use global_asm! and _start. [Runner](../../reference/runner.md#user-programs) builds /bin programs. Normal boot runs /bin/hello through program::run and prints Hello from ring 3!.

### User-provided memory {#user-mem}

user_mem::check requires the complete range in the lower half without overflow, every page USER_ACCESSIBLE and, for writes, WRITABLE; otherwise EFAULT. Empty lower-half ranges are valid. UserSpace copy_from_user/copy_to_user in `src/memory.rs` validate then copy through the physical mapping. check_user first materializes reserved permitted pages. Never dereference raw program pointers.

## Code {#code}

`src/user.rs`: run, run_with, start, Stop, initial_frame, Trap, user_trap, user_fault, map_on_demand. `src/scheduler.rs`: return_to_user/exit_current. `src/syscall.rs`: init, entry, dispatch. `osel-core/src/syscall.rs`: SYSCALLS, number, args, encode, return_path, Utf8Decoder. `src/program.rs`: run. `src/fpu.rs` and `osel-core/src/fpu.rs`: hardware/state. `src/interrupts.rs`: entry_stub/through_stub. `src/gdt.rs`: selectors and stack slot. `osel-core/src/trap.rs`: frame and exception names; `osel-core/src/user_mem.rs`: check/Access. First program: `user/hello/src/main.rs`.

## Invariants {#invariants}

Launch with enabled interrupts and no held locks. Frame lies immediately below the thread's kernel-stack top, used by RSP0 and CURRENT_STACK_TOP. Ring-0 exceptions retain their original CPU frame. Access user memory only through checked copy methods.

## Locking {#locks}

Stubs/user_trap run interrupt-masked and hold no locks. Dispatch is preemptible with interrupts enabled; release all guards before return or nonreturning exit_current.

## Pitfalls {#pitfalls}

Clear user DF before Rust: user_trap uses cld, LLVM interrupt entry does so automatically, syscall masks it, and switch_to restores each thread's flags. Keep interrupts disabled between frame placement and iretq. Ring-0 entry from user sets SS to zero; switching reloads kernel SS/DS/ES. QEMU can give #GP rather than #SS for noncanonical RSP, so tests accept both.

Enabling interrupts before the frame is built lets a timer write to the user stack; zero RSP can fault at 0xFFFF_FFFF_FFFF_FFF8. Machine-code tests verify order. Noncanonical sysret RCX can #GP in ring 0; validate first. TF sysret traps one instruction earlier than iret, so use iret for TF.

Software-emulated QEMU may only set MXCSR rather than deliver #XM; tests accept both and check flags. fxsave lists x87 registers from ST(0), not physical index. Use SSE2, not SSE4.1 pinsrq unsupported by the default CPU. Intel bare symbols mean memory operands; calculate lengths with lea/sub. Test code copied from kernel into ELF must be position independent.

## Tests {#tests}

Host tests cover frame offsets/sanitization/reports, ranges and holes, call ABI and UTF-8 against String::from_utf8_lossy. Kernel tests cover registers, initial/empty stacks, timer/DF, 16 forbidden actions, forced ring 3 despite hostile CS/IOPL, stack guard, 100 leak-free runs, preserved kernel mappings, checked copies, call-clobber registers, EFAULT with no output, unknown calls/descriptors, 200000 stackless calls, DF/AC/TF, invalid return addresses and x87/SSE state.

## Decisions {#decisions}

[0011](../../decisions/0011-user-entry.md), [0012](../../decisions/0012-syscall-entry-and-user-programs.md), [0013](../../decisions/0013-fpu-state.md), [0015](../../decisions/0015-threads.md).

_Checked against code: 2026-10-13, stage-2 time work._
