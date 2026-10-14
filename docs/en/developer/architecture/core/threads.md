---
description: Per-thread kernel stacks, context switching, timer preemption, processes, clone, waits and cooperative teardown.
---

# Threads and scheduler

Language: [Русский](../../../../ru/developer/architecture/core/threads.md) · [English](threads.md)

**Audience:** developers changing threads, processes, switching or timers.

**Summary:** each thread owns a guarded kernel stack with user TrapFrame at its top. switch_to saves callee-saved registers/RFLAGS and changes stacks; surrounding code changes CR3/RSP0/FS/x87/SSE. Timer schedules round-robin every tick. Threads share their Process space/files and clone creates more.

## Behavior {#how}

### Threads {#threads}

Thread in `src/scheduler.rs` records ID, saved stack pointer, kernel stack, optional Process, kernel function, FpuState, FS base, ready/running/waiting/dead state and run count. Thread 0 is immortal boot/test/wait code on the bootloader stack. Idle runs sti;hlt only when no ready work exists and is outside the queue. spawn_user receives a frame/FPU/FS; spawn_kernel runs a function and exits on return.

STACKS_START uses level-4 entry 386 with 1024 slots of STACK_SIZE 64 KB plus an unmapped guard under each. StackSlots in `osel-core/src/sched.rs` owns positions; map_range supplies pages. Shared upper-half tables make stacks visible in every address space.

### User frame at stack top {#frame}

Top is 16-byte aligned; the 176-byte TrapFrame lies directly below it. User exceptions use TSS RSP0, syscall_entry uses CURRENT_STACK_TOP, and first user entry uses the frame placed by spawn_user/return_to_user. Timer entry from user saves only the CPU frame at the top; x86-interrupt handler and switch_to preserve the remaining registers.

### Switching {#switch}

schedule masks interrupts, briefly locks SCHEDULER and selects RoundRobin's head or idle. A still-running current thread goes to the queue tail. Release the lock before changing stacks.

prepare_switch saves/restores fxsave64 state, retains user FS or kernel-thread CR3, loads zero FS selector then incoming base, changes CR3 only when different, and sets TSS RSP0/CURRENT_STACK_TOP. switch_to saves callee-saved registers/RFLAGS, records outgoing RSP, loads incoming RSP and returns on that stack with the previous thread in RAX. finish_switch reloads kernel SS/DS/ES and frees a dead predecessor's stack/process after the CR3 change. Restore this thread's original interrupt state.

A new thread first returns through thread_start/thread_entry: finish_switch, then enable interrupts and run the kernel function or return to ring 3.

### Preemption {#preemption}

After EOI, timer calls scheduler::tick every 10 ms. It can preempt user code or interrupt-enabled syscall code. IrqSpinlock suppresses timer arrival, so no thread switches while holding one. sched_yield and block_current invoke schedule directly.

### Processes, termination and waiting {#process}

Process owns a monotonic PID, weak parent, strong children, space/files, live-thread count, kill flag, Exit (Stop/frame/FPU) and wait queues. Root programs and children use one PID sequence; thread IDs are independent.

exit_thread handles exit for only the current thread; the last publishes the process result. exit_current records the first whole-process result, flags termination and wakes blocked threads; exit_group, user faults and kill use it. Process::kill wakes terminal/pipe/futex/time/epoll/child waits, which return EINTR so local Vec/Arc destructors run before stack deletion. Check kill before first user entry, syscall return, demand-fault return and ring-3 timer return.

The final thread kills and waits for descendants without holding locks. No init adoption exists; the tree lifetime is bounded by its parent. Child cleanup closes descriptors, releases vfork and sets completed only afterward. A kernel-launched program retains descriptors for its caller. Process::wait atomically checks/registers/blocks and consumes one result. wait_child handles wait4 for a PID or any child, reports status and removes the entry. Zombies retain Process/address space until wait, but files are closed and stacks freed. The bound is 128 unreaped children, not a physical-memory limit.

user::start sanitizes ring 3, rejects noncanonical RIP with EFAULT and returns without waiting. run/run_with launch, wait and return owned space/files.

### Thread clone {#clone}

`src/syscall.rs` uses spawn_in on the same Arc<Process>. Memory/files are shared, kernel stack independent. Copy frame with child RAX=0 and RSP=child_stack, preserving Go pointers in R9/R12/R13. Copy x87/SSE and inherit FS or set it from CLONE_SETTLS. See [flags/errors](../../reference/syscalls.md).

### Child process and execve {#child-processes}

clone(CLONE_VM | CLONE_VFORK | SIGCHLD) uses scheduler::vfork: new PID and copied FdTable with shared open descriptions, temporarily shared Arc<IrqSpinlock<UserSpace>>, and only the calling parent thread blocked. Wake after child exec success or exit, including launch-error exit.

execve copies path/argv/envp, checks permissions and fully loads ELF/stack before commitment. Failure retains the old image. scheduler::exec then replaces the space, switches CR3, resets FS/FPU, closes CLOEXEC and releases the parent wait; the frame gets new entry/stack. Multithreaded exec returns EINVAL.

MM uses UnsafeCell so replacement does not carry a guard across CR3. Safety requires one CPU, a single-threaded exec process, interrupts masked and no old-MM guard. Parent's Arc retains its own memory. Multi-CPU/multithreaded exec needs redesign.

Go 1.27.1's locally inspected/QEMU-tested path uses ENOSYS for pidfd/waitid and falls back to clone/wait4. No Linux strace was collected; repeat `tests/go_exec.rs` after updating Go.

## Time and sleep {#time}

100 Hz ticks convert to nanoseconds through osel_core::clock. time::monotonic_ns is uptime; realtime_ns adds the CMOS Unix base read by time::init, respecting BCD/binary and 12/24-hour formats. Coarse/raw/boottime variants use the same clocks.

nanosleep and futex/epoll timeouts register deadlines in SLEEPERS and block. Timer wake_due wakes expired entries. Check/register/block with interrupts masked to avoid lost wakes. Minimum effective sleep is one 10 ms tick; Go time.Sleep relies on runtime timers/futex rather than nanosleep precision.

## futex {#futex}

FUTEX_WAIT checks the expected word and sleeps; FUTEX_WAKE wakes waiters. Keys combine process space and address, avoiding same-address collisions between processes. Check/enqueue atomically on one CPU with interrupts masked. Relative timeout shares SLEEPERS. Spurious wakeups are harmless because Go rechecks conditions.

## Code {#code}

`src/scheduler.rs`: Thread/Process/Exit, init, spawn_user/spawn_kernel, schedule, prepare_switch/switch_to/finish_switch, thread_start/return_to_user, tick/block_current/wake/yield_now/exit_current, switches/runs. `osel-core/src/sched.rs`: SchedPolicy/RoundRobin/StackSlots. `src/user.rs`: start/run/run_with/traps. `src/time.rs`: clocks and waits; `osel-core/src/clock.rs`, `osel-core/src/sleeper.rs`, `osel-core/src/futex.rs`: pure logic. `src/futex.rs`: hardware wait/wake. `src/syscall.rs`: stack entry/yield/kill checks. `src/interrupts.rs`: timer tick.

## Invariants {#invariants}

Mask interrupts during switching and release SCHEDULER before switch_to. Never switch while holding IrqSpinlock; Process::wait requires enabled interrupts. Reclaim dead stacks only after changing CR3. Preallocate queue capacity for all stack slots, avoiding IRQ allocation. Thread 0 never exits. MM/FILES protect shared state. Cancel blocked Rust calls before deleting their stacks. Publish completed after all threads/descendants and child descriptor cleanup, ensuring pipe EOF before wait4.

## Locking {#locks}

SCHEDULER/STACK_SLOTS guard brief operations, including timer use. PROCESS_EXIT/PROCESS_WAITER guard results. finish_switch removes dead threads then frees stack/process using MEMORY/STACK_SLOTS. MM guards only nonblocking work; physical mapping avoids nested user-pointer faults. FILES is released before read/write. CHILDREN/CHILD_WAITERS/VFORK never cross block_current/wait. See [inventory](locking.md#locks).

## Pitfalls {#pitfalls}

Reload SS/DS/ES after a user-origin switch. Load zero FS selector before setting its base. Preallocate VecDeque so timer never allocates. Context tests must mutate XMM and memory, not merely observe clean defaults. Plain OFFSET Spinlock can be preempted: seekable file implementations must never sleep under it; terminals are nonseekable. Kernel stack overflow becomes double fault because page faults use the overflowing stack. Deleting blocked stacks loses destructors; wake/EINTR/unwind normally first. Zombies retain full spaces: reap promptly, since child count is not a memory limit.

## Tests {#tests}

Host tests check RoundRobin/model/no starvation, guarded slots and double release. Scheduler tests use two check_context programs with changing registers/flags/XMM/MXCSR/FS over at least 150 switches; both counters advance, then kill succeeds. Cover infinite-loop preemption/kill, blocked input kill, yield, alternating kernel threads, 100 leak-free threads and guards. All user/call/FPU tests use thread launch/wait. `tests/go_exec.rs` and `user-go/goexec/main.go` cover clear/PATH, argv/envp, code 37, missing executable, repeats and transfers exceeding pipe capacity with heap/frame recovery.

## Decisions {#decisions}

[0015](../../decisions/0015-threads.md), [0017](../../decisions/0017-child-processes.md), [0011](../../decisions/0011-user-entry.md), [0013](../../decisions/0013-fpu-state.md).

_Checked against code: 2026-10-13, working branch for stages 2.5 and 3._
