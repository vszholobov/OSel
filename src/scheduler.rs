//! scheduler module: threads, processes and switching between them
//!
//! Every thread has its own kernel stack. A user thread's registers live in a `TrapFrame`
//! at the top of that stack: traps and system calls from ring 3 land there, and the thread
//! goes back to ring 3 from there. `switch_to` swaps kernel stacks and saves only the
//! callee-saved registers and RFLAGS; everything else a thread needs is either on its stack
//! or switched around it (CR3, RSP0, FS base, x87 and SSE). The boot code is thread 0 on
//! the bootloader's stack; an idle thread waits in `hlt` when nothing else is ready.
//!
//! Switching happens with interrupts off, and the scheduler lock is never held across it:
//! one CPU, so turning interrupts off is enough to keep anyone else out.

use crate::fpu;
use crate::gdt;
use crate::memory::{self, PAGE_SIZE, UserSpace};
use crate::sync::IrqSpinlock;
use crate::user::{Stop, Trap};
use alloc::boxed::Box;
use alloc::collections::BTreeMap;
use alloc::sync::{Arc, Weak};
use alloc::vec::Vec;
use core::arch::naked_asm;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use osel_core::errno::{Errno, KResult};
use osel_core::file::FdTable;
use osel_core::fpu::FpuState;
use osel_core::sched::{RoundRobin, SchedPolicy, StackSlot, StackSlots};
use osel_core::trap::{TRAP_FRAME_WORDS, TrapFrame};
use x86_64::VirtAddr;
use x86_64::instructions::interrupts;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{PageTableFlags, PhysFrame};

pub use osel_core::sched::ThreadId;

/// Kernel stacks live in a level 4 entry of their own, after the heap and a test area
pub const STACKS_START: u64 = 0xffff_c100_0000_0000;
/// Kernel stack of a thread; Go's threads spend little time in the kernel
pub const STACK_SIZE: u64 = 64 * 1024;
/// The most threads at once: one kernel stack slot each
pub const MAX_THREADS: usize = 1024;
const STACK_COUNT: usize = MAX_THREADS;
/// Bytes of the `TrapFrame` at the top of a kernel stack
const FRAME_SIZE: u64 = (TRAP_FRAME_WORDS * 8) as u64;
const _: () = assert!(FRAME_SIZE % 16 == 0, "the frame keeps the stack 16 byte aligned");

/// RFLAGS of a thread that has never run: interrupts off until it is set up
const INITIAL_RFLAGS: u64 = 0x2;

/// The thread 0: the boot code on the bootloader's stack
const BOOT_THREAD: ThreadId = 0;

static SCHEDULER: IrqSpinlock<Option<Scheduler>> = IrqSpinlock::new("SCHEDULER", None);
static STACK_SLOTS: IrqSpinlock<Option<StackSlots>> = IrqSpinlock::new("STACK_SLOTS", None);
/// Top of the running thread's kernel stack: where `syscall` puts its frame
pub(crate) static mut CURRENT_STACK_TOP: u64 = 0;
/// The running thread; changed only with interrupts off
static mut CURRENT: *mut Thread = core::ptr::null_mut();
/// Level 4 table of the kernel, for kernel threads; read without taking `MEMORY`
static mut KERNEL_LEVEL_4: Option<PhysFrame> = None;
static READY: AtomicBool = AtomicBool::new(false);
/// Switches between two different threads since boot
static SWITCHES: AtomicU64 = AtomicU64::new(0);

/// How a process ended and its state at that moment, for whoever waits for it
pub struct Exit {
    pub stop: Stop,
    /// The registers when the last thread left ring 3 through a trap or a system call;
    /// zeros for a thread killed while it ran
    pub frame: TrapFrame,
    pub fpu: FpuState,
}

/// A running program: its address space and descriptors, shared by all of its threads
/// (`clone` makes more). The locks guard every access from a thread or the page-fault
/// handler; see the locking section of `docs/ru/developer/architecture/core/threads.md`.
pub struct Process {
    space: UnsafeCell<Arc<IrqSpinlock<UserSpace>>>,
    files: IrqSpinlock<FdTable>,
    level_4: AtomicU64,
    pid: u64,
    parent: Weak<Process>,
    children: IrqSpinlock<BTreeMap<u64, Arc<Process>>>,
    child_waiters: IrqSpinlock<Vec<ThreadId>>,
    vfork: IrqSpinlock<Option<Arc<Vfork>>>,
    completed: AtomicBool,
    killed: AtomicBool,
    /// Live threads; the last one out records the exit and wakes the waiter
    threads: AtomicU64,
    exit: IrqSpinlock<Option<Exit>>,
    waiter: IrqSpinlock<Option<ThreadId>>,
}

struct Vfork {
    done: AtomicBool,
    caller: ThreadId,
}
static NEXT_PID: AtomicU64 = AtomicU64::new(1);

// SAFETY: the address space and the descriptors are only reached through the locks; the
// waiter takes them back after the process died
unsafe impl Sync for Process {}
unsafe impl Send for Process {}

impl Process {
    pub fn new(space: UserSpace, files: FdTable) -> Arc<Process> {
        let level_4 = space.level_4_frame();
        Arc::new(Process {
            space: UnsafeCell::new(Arc::new(IrqSpinlock::new("MM", space))),
            files: IrqSpinlock::new("FILES", files),
            level_4: AtomicU64::new(level_4.start_address().as_u64()),
            pid: NEXT_PID.fetch_add(1, Ordering::SeqCst),
            parent: Weak::new(),
            children: IrqSpinlock::new("CHILDREN", BTreeMap::new()),
            child_waiters: IrqSpinlock::new("CHILD_WAITERS", Vec::with_capacity(MAX_THREADS)),
            vfork: IrqSpinlock::new("VFORK", None),
            completed: AtomicBool::new(false),
            killed: AtomicBool::new(false),
            threads: AtomicU64::new(1),
            exit: IrqSpinlock::new("PROCESS_EXIT", None),
            waiter: IrqSpinlock::new("PROCESS_WAITER", None),
        })
    }

    /// The address space, locked for the call. Held only across non-blocking work, and the
    /// kernel never dereferences a program address while holding it (it goes through the
    /// physical map), so the page-fault handler may take it too.
    pub(crate) fn lock_space(&self) -> crate::sync::IrqSpinlockGuard<'_, UserSpace> {
        // Only single-threaded exec can replace this Arc, with interrupts disabled and
        // no outstanding MM guard. All other accesses merely borrow the shared lock.
        unsafe { &*self.space.get() }.lock()
    }

    /// The descriptors, locked for the call. Held only to look up or change a descriptor,
    /// never across a read or write that may block.
    pub(crate) fn lock_files(&self) -> crate::sync::IrqSpinlockGuard<'_, FdTable> {
        self.files.lock()
    }

    /// An id unique to this address space while it lives: its level 4 table frame. Used to
    /// key `futex` waits so two processes with the same address don't collide.
    pub(crate) fn space_id(&self) -> u64 {
        self.level_4.load(Ordering::SeqCst)
    }

    /// Ends the process the next time one of its threads would return to ring 3
    pub fn kill(&self) {
        self.killed.store(true, Ordering::SeqCst);
        let mut guard = SCHEDULER.lock();
        if let Some(scheduler) = guard.as_mut() {
            for (&id, thread) in &mut scheduler.threads {
                if thread.state == State::Blocked
                    && thread
                        .process
                        .as_deref()
                        .is_some_and(|p| core::ptr::eq(p, self))
                {
                    thread.state = State::Ready;
                    scheduler.policy.add(id);
                }
            }
        }
    }

    pub fn is_killed(&self) -> bool {
        self.killed.load(Ordering::SeqCst)
    }

    /// Whether the process ended and nobody took its exit yet
    pub fn has_exited(&self) -> bool {
        self.completed.load(Ordering::SeqCst) && self.exit.lock().is_some()
    }

    /// Waits until the whole process ended; one waiter at a time
    pub fn wait(&self) -> Exit {
        assert!(interrupts::are_enabled(), "waiting with interrupts off or a lock held");
        loop {
            // checked and marked with interrupts off: the exit can't slip in between
            interrupts::disable();
            // taken out first: the lock goes before interrupts come back
            let exit = if self.completed.load(Ordering::SeqCst) {
                self.exit.lock().take()
            } else {
                None
            };
            if let Some(exit) = exit {
                interrupts::enable();
                return exit;
            }
            *self.waiter.lock() = Some(current_id());
            block_current();
            interrupts::enable();
        }
    }

    /// The space and the descriptors back, after the process ended
    pub fn into_parts(self) -> (UserSpace, FdTable) {
        let mm = Arc::into_inner(self.space.into_inner())
            .expect("a child still shares the address space");
        (mm.into_inner(), self.files.into_inner())
    }

    pub(crate) fn pid(&self) -> u64 {
        self.pid
    }
    pub(crate) fn parent_pid(&self) -> u64 {
        self.parent.upgrade().map_or(0, |p| p.pid)
    }

    fn release_vfork(&self) {
        let wait = self.vfork.lock().take();
        if let Some(wait) = wait {
            wait.done.store(true, Ordering::SeqCst);
            wake(wait.caller);
        }
    }
}

/// A separate descriptor table and a shared address space until exec or exit.
pub(crate) fn vfork(frame: TrapFrame, fpu: FpuState, fs_base: u64) -> KResult<u64> {
    let parent = unsafe { current_process_arc() };
    if parent.children.lock().len() >= 128 {
        return Err(Errno::EAGAIN);
    }
    let wait = Arc::new(Vfork {
        done: AtomicBool::new(false),
        caller: current_id(),
    });
    let pid = NEXT_PID.fetch_add(1, Ordering::SeqCst);
    let child = Arc::new(Process {
        space: UnsafeCell::new(unsafe { &*parent.space.get() }.clone()),
        files: IrqSpinlock::new("FILES", parent.lock_files().clone()),
        level_4: AtomicU64::new(parent.space_id()),
        pid,
        parent: Arc::downgrade(&parent),
        children: IrqSpinlock::new("CHILDREN", BTreeMap::new()),
        child_waiters: IrqSpinlock::new("CHILD_WAITERS", Vec::with_capacity(MAX_THREADS)),
        vfork: IrqSpinlock::new("VFORK", Some(wait.clone())),
        completed: AtomicBool::new(false),
        killed: AtomicBool::new(false),
        threads: AtomicU64::new(1),
        exit: IrqSpinlock::new("PROCESS_EXIT", None),
        waiter: IrqSpinlock::new("PROCESS_WAITER", None),
    });
    let thread = new_thread(Some(child.clone()), None, frame, fpu, fs_base)?;
    interrupts::disable();
    // Other parent threads could have spawned while the kernel stack was allocated.
    if parent.children.lock().len() >= 128 || parent.is_killed() {
        interrupts::enable();
        return Err(Errno::EAGAIN);
    }
    parent.children.lock().insert(pid, child);
    SCHEDULER.lock().as_mut().unwrap().add(thread);
    while !wait.done.load(Ordering::SeqCst) && !parent.is_killed() {
        block_current();
    }
    interrupts::enable();
    Ok(pid)
}

/// Commits an already validated ELF. Failure before this point preserves the old image.
pub(crate) fn exec(space: UserSpace) -> KResult<()> {
    let process = unsafe { current_process() };
    if process.threads.load(Ordering::SeqCst) != 1 {
        return Err(Errno::EINVAL);
    }
    let level_4 = space.level_4_frame();
    let new = Arc::new(IrqSpinlock::new("MM", space));
    interrupts::disable();
    let old = unsafe { core::mem::replace(&mut *process.space.get(), new) };
    process
        .level_4
        .store(level_4.start_address().as_u64(), Ordering::SeqCst);
    unsafe { Cr3::write(level_4, Cr3::read().1) };
    x86_64::registers::model_specific::FsBase::write(VirtAddr::zero());
    unsafe { (*CURRENT).fs_base = 0 };
    fpu::restore(&FpuState::initial());
    process.lock_files().close_on_exec();
    drop(old);
    process.release_vfork();
    interrupts::enable();
    Ok(())
}

/// Copies the status before reaping, atomically against other waiters and munmap.
pub(crate) fn wait_child(pid: i64, nohang: bool, mut report: impl FnMut(u32) -> KResult<()>) -> KResult<Option<(u64, u32)>> {
    if pid != -1 && pid <= 0 {
        return Err(Errno::EINVAL);
    }
    let process = unsafe { current_process() };
    loop {
        interrupts::disable();
        let mut children = process.children.lock();
        let found = children
            .iter()
            .find(|(id, p)| (pid == -1 || **id == pid as u64) && p.has_exited())
            .map(|(&id, p)| {
                let status = match p.exit.lock().as_ref().unwrap().stop {
                    Stop::Exit(code) => (code as u32) << 8,
                    Stop::Killed => 9,
                    Stop::Fault(_) => 11,
                };
                (id, status)
            });
        let exists = children.keys().any(|&id| pid == -1 || id == pid as u64);
        if let Some((_, status)) = found {
            if let Err(error) = report(status) {
                drop(children);
                interrupts::enable();
                return Err(error);
            }
        }
        let reaped = found.and_then(|(id, _)| children.remove(&id));
        drop(children);
        drop(reaped);
        if found.is_some() || !exists || nohang || process.is_killed() {
            interrupts::enable();
            return if !exists {
                Err(Errno::ECHILD)
            } else if process.is_killed() {
                Err(Errno::EINTR)
            } else {
                Ok(found)
            };
        }
        process.child_waiters.lock().push(current_id());
        block_current();
        process
            .child_waiters
            .lock()
            .retain(|&id| id != current_id());
        interrupts::enable();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Running,
    Blocked,
    Dead,
}

/// A kernel stack in its slot, mapped below an unmapped guard page
struct KernelStack {
    slot: StackSlot,
}

impl KernelStack {
    fn new() -> KResult<KernelStack> {
        let slot = STACK_SLOTS.lock().as_mut().expect("scheduler is not initialized").allocate().ok_or(Errno::ENOMEM)?;
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_EXECUTE;
        let mapped = memory::with_memory(|m| {
            memory::map_range(&mut m.mapper, &mut m.frame_allocator, VirtAddr::new(slot.bottom), STACK_SIZE, flags)
        });
        if mapped.is_err() {
            STACK_SLOTS.lock().as_mut().unwrap().free(slot.index);
            return Err(Errno::ENOMEM);
        }
        Ok(KernelStack { slot })
    }
}

impl Drop for KernelStack {
    fn drop(&mut self) {
        memory::with_memory(|m| {
            memory::unmap_range(&mut m.mapper, &mut m.frame_allocator, VirtAddr::new(self.slot.bottom), STACK_SIZE)
        })
        .expect("a kernel stack is a valid range");
        // every address space shares these tables: their TLB entries go with the next CR3
        // switch, the current one's right now
        x86_64::instructions::tlb::flush_all();
        STACK_SLOTS.lock().as_mut().unwrap().free(self.slot.index);
    }
}

struct Thread {
    id: ThreadId,
    /// Saved by `switch_to` while the thread does not run
    rsp: u64,
    /// `None` for the boot thread, which runs on the bootloader's stack
    stack: Option<KernelStack>,
    /// `None` for kernel threads
    process: Option<Arc<Process>>,
    /// What a kernel thread runs
    entry: Option<fn()>,
    fpu: FpuState,
    fs_base: u64,
    /// The table a kernel thread had active when it was switched out: it may be working in
    /// a program's address space
    level_4: Option<PhysFrame>,
    state: State,
    /// Times the thread was switched to
    runs: u64,
}

impl Thread {
    fn stack_top(&self) -> u64 {
        self.stack.as_ref().map_or(0, |stack| stack.slot.top)
    }
}

struct Scheduler {
    threads: BTreeMap<ThreadId, Box<Thread>>,
    policy: RoundRobin,
    current: ThreadId,
    idle: ThreadId,
    next_id: ThreadId,
}

impl Scheduler {
    fn add(&mut self, mut thread: Box<Thread>) -> ThreadId {
        let id = self.next_id;
        self.next_id += 1;
        thread.id = id;
        let ready = thread.state == State::Ready;
        self.threads.insert(id, thread);
        if ready {
            self.policy.add(id);
        }
        id
    }
}

/// Creates thread 0 for the boot code and the idle thread; switching starts with the
/// first timer tick after `osel::init` turns interrupts on
pub fn init() {
    *STACK_SLOTS.lock() = Some(StackSlots::new(STACKS_START, STACK_COUNT, STACK_SIZE, PAGE_SIZE));
    unsafe { KERNEL_LEVEL_4 = Some(memory::with_memory(|m| m.kernel_level_4)) };
    let boot = Box::new(Thread {
        id: BOOT_THREAD,
        rsp: 0,
        stack: None,
        process: None,
        entry: None,
        fpu: FpuState::initial(),
        fs_base: 0,
        level_4: None,
        state: State::Running,
        runs: 1,
    });
    let mut scheduler =
        Scheduler {
        threads: BTreeMap::new(),
        // every thread has a stack slot, so this many never makes the queue grow in the timer
        policy: RoundRobin::with_capacity(STACK_COUNT + 1),
        current: BOOT_THREAD,
        idle: 0,
        next_id: BOOT_THREAD,
    };
    scheduler.add(boot);
    unsafe { CURRENT = &mut **scheduler.threads.get_mut(&BOOT_THREAD).unwrap() };
    let idle = new_thread(None, Some(idle), TrapFrame::default(), FpuState::initial(), 0).expect("no stack for the idle thread");
    let idle = {
        let mut idle = idle;
        // never in the run queue: picked only when nothing else is ready
        idle.state = State::Blocked;
        scheduler.add(idle)
    };
    scheduler.idle = idle;
    *SCHEDULER.lock() = Some(scheduler);
    READY.store(true, Ordering::SeqCst);
}

fn idle() {
    loop {
        interrupts::enable_and_hlt();
    }
}

/// A thread whose first switch lands in `thread_start`: for a user thread with `frame` at
/// the top of its stack, for a kernel thread calling `entry`
fn new_thread(process: Option<Arc<Process>>, entry: Option<fn()>, frame: TrapFrame, fpu: FpuState, fs_base: u64) -> KResult<Box<Thread>> {
    let stack = KernelStack::new()?;
    let top = stack.slot.top;
    let frame_at = top - FRAME_SIZE;
    unsafe { (frame_at as *mut TrapFrame).write(frame) };
    // what `switch_to` pops: RFLAGS, r15, r14, r13, r12, rbx, rbp, then the return address
    let switch_area = [INITIAL_RFLAGS, 0, 0, 0, 0, 0, 0, thread_start as *const () as u64];
    let rsp = frame_at - (switch_area.len() * 8) as u64;
    for (index, word) in switch_area.iter().enumerate() {
        unsafe { (rsp as *mut u64).add(index).write(*word) };
    }
    Ok(Box::new(Thread { id: 0, rsp, stack: Some(stack), process, entry, fpu, fs_base, level_4: None, state: State::Ready, runs: 0 }))
}

/// Starts a thread of `process` in ring 3 with `frame`, its x87 and SSE state and FS base
pub fn spawn_user(process: Arc<Process>, frame: TrapFrame, fpu: FpuState, fs_base: u64) -> KResult<ThreadId> {
    let thread = new_thread(Some(process), None, frame, fpu, fs_base)?;
    Ok(SCHEDULER.lock().as_mut().expect("scheduler is not initialized").add(thread))
}

/// `clone`: another thread of the running process, sharing its space and descriptors.
/// The caller counts it in `threads` first, so the process can't end between.
pub fn spawn_in(process: Arc<Process>, frame: TrapFrame, fpu: FpuState, fs_base: u64) -> KResult<ThreadId> {
    process.threads.fetch_add(1, Ordering::SeqCst);
    match new_thread(Some(process.clone()), None, frame, fpu, fs_base) {
        Ok(thread) => Ok(SCHEDULER.lock().as_mut().expect("scheduler is not initialized").add(thread)),
        Err(error) => {
            process.threads.fetch_sub(1, Ordering::SeqCst);
            Err(error)
        }
    }
}

/// Starts a kernel thread running `entry`; it ends when `entry` returns
pub fn spawn_kernel(entry: fn()) -> KResult<ThreadId> {
    let thread = new_thread(None, Some(entry), TrapFrame::default(), FpuState::initial(), 0)?;
    Ok(SCHEDULER.lock().as_mut().expect("scheduler is not initialized").add(thread))
}

pub fn current_id() -> ThreadId {
    unsafe { CURRENT.as_ref() }.map_or(BOOT_THREAD, |thread| thread.id)
}

/// The process of the running thread
///
/// # Safety
/// Only from the thread itself, while it runs.
pub(crate) unsafe fn current_process() -> &'static Process {
    let thread = unsafe { CURRENT.as_ref() }.expect("no thread runs");
    thread.process.as_deref().expect("a kernel thread has no process")
}

/// A new reference to the running thread's process
///
/// # Safety
/// Only from the thread itself, while it runs.
pub(crate) unsafe fn current_process_arc() -> Arc<Process> {
    let thread = unsafe { CURRENT.as_ref() }.expect("no thread runs");
    thread.process.clone().expect("a kernel thread has no process")
}

/// The address space of the running thread's process, locked for the call
///
/// # Safety
/// Only from the thread itself, while it runs.
pub(crate) unsafe fn current_space() -> crate::sync::IrqSpinlockGuard<'static, UserSpace> {
    unsafe { current_process() }.lock_space()
}

/// The descriptors of the running thread's process, locked for the call
///
/// # Safety
/// As `current_space`.
pub(crate) unsafe fn current_files() -> crate::sync::IrqSpinlockGuard<'static, FdTable> {
    unsafe { current_process() }.lock_files()
}

/// Number of switches between different threads since boot
pub fn switches() -> u64 {
    SWITCHES.load(Ordering::Relaxed)
}

/// How many times thread `id` was switched to, `None` once it is gone
pub fn runs(id: ThreadId) -> Option<u64> {
    SCHEDULER.lock().as_ref()?.threads.get(&id).map(|thread| thread.runs)
}

/// Gives the CPU to the next ready thread, if there is one. Comes back when this thread
/// is picked again, with interrupts as they were.
pub fn schedule() {
    if !READY.load(Ordering::SeqCst) {
        return;
    }
    let enabled = interrupts::are_enabled();
    interrupts::disable();
    let switch = {
        let mut guard = SCHEDULER.lock();
        let scheduler = guard.as_mut().unwrap();
        let prev_id = scheduler.current;
        let prev: *mut Thread = &mut **scheduler.threads.get_mut(&prev_id).unwrap();
        let prev_state = unsafe { (*prev).state };
        if prev_state == State::Running && prev_id != scheduler.idle {
            unsafe { (*prev).state = State::Ready };
            scheduler.policy.add(prev_id);
        }
        // skip ids of reaped threads still queued: a killed process leaves them behind
        let next_id = loop {
            match scheduler.policy.next() {
                Some(id)
                    if scheduler
                        .threads
                        .get(&id)
                        .is_some_and(|t| t.state == State::Ready) =>
                {
                    break id;
                }
                Some(_) => continue,
                None => break scheduler.idle,
            }
        };
        if next_id == prev_id {
            unsafe { (*prev).state = State::Running };
            None
        } else {
            if prev_state == State::Running && prev_id == scheduler.idle {
                unsafe { (*prev).state = State::Blocked };
            }
            let next: *mut Thread = &mut **scheduler.threads.get_mut(&next_id).unwrap();
            unsafe {
                (*next).state = State::Running;
                (*next).runs += 1;
            }
            scheduler.current = next_id;
            Some((prev, next))
        }
    };
    if let Some((prev, next)) = switch {
        SWITCHES.fetch_add(1, Ordering::Relaxed);
        unsafe {
            prepare_switch(&mut *prev, &mut *next);
            CURRENT = next;
            let came_from = switch_to(&mut (*prev).rsp, (*next).rsp, prev);
            finish_switch(came_from);
        }
    }
    if enabled {
        interrupts::enable();
    }
}

/// What has to follow a thread around: the outgoing user thread's registers that the
/// stack doesn't hold are saved, the incoming one's loaded
unsafe fn prepare_switch(prev: &mut Thread, next: &mut Thread) {
    use x86_64::instructions::segmentation::{FS, Segment};
    use x86_64::registers::model_specific::FsBase;
    use x86_64::structures::gdt::SegmentSelector;

    // kernel threads too: the kernel itself doesn't use x87 and SSE, but its tests do
    fpu::save(&mut prev.fpu);
    fpu::restore(&next.fpu);
    match prev.process {
        Some(_) => prev.fs_base = FsBase::read().as_u64(),
        None => prev.level_4 = Some(Cr3::read().0),
    }
    let level_4 = match &next.process {
        Some(process) => {
            // a program may have loaded a selector into FS; loading one changes the base,
            // so the null selector first, then the base
            unsafe { FS::set_reg(SegmentSelector(0)) };
            FsBase::write(VirtAddr::new(next.fs_base));
            PhysFrame::containing_address(x86_64::PhysAddr::new(process.space_id()))
        }
        None => next.level_4.or(unsafe { KERNEL_LEVEL_4 }).expect("scheduler is not initialized"),
    };
    let (active, flags) = Cr3::read();
    if active != level_4 {
        unsafe { Cr3::write(level_4, flags) };
    }
    if let Some(stack) = &next.stack {
        unsafe {
            gdt::kernel_stack_slot().write_unaligned(stack.slot.top);
            CURRENT_STACK_TOP = stack.slot.top;
        }
    }
}

/// Runs on the incoming thread right after the switch: the segment registers the
/// previous thread may have left from ring 3, and the stack of a thread that died
unsafe fn finish_switch(prev: *mut Thread) {
    gdt::reload_data_segments();
    if unsafe { (*prev).state } == State::Dead {
        let id = unsafe { (*prev).id };
        let dead = SCHEDULER.lock().as_mut().unwrap().threads.remove(&id);
        // freed outside the scheduler lock: the stack and maybe the process go now, after
        // CR3 moved away from them
        drop(dead);
    }
}

/// Saves the callee-saved registers and RFLAGS on this stack, stores the stack pointer at
/// `prev_rsp`, switches to `next_rsp` and returns there with `prev` in RAX
#[unsafe(naked)]
unsafe extern "C" fn switch_to(prev_rsp: *mut u64, next_rsp: u64, prev: *mut Thread) -> *mut Thread {
    naked_asm!(
        "push rbp",
        "push rbx",
        "push r12",
        "push r13",
        "push r14",
        "push r15",
        "pushfq",
        "mov [rdi], rsp",
        "mov rsp, rsi",
        "popfq",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop rbx",
        "pop rbp",
        "mov rax, rdx",
        "ret",
    )
}

/// Where a new thread's first switch returns to, with the previous thread in RAX and the
/// stack pointer right below the frame, 16 byte aligned
#[unsafe(naked)]
unsafe extern "C" fn thread_start() {
    naked_asm!("mov rdi, rax", "call {entry}", "ud2", entry = sym thread_entry)
}

extern "C" fn thread_entry(prev: *mut Thread) -> ! {
    unsafe { finish_switch(prev) };
    let thread = unsafe { &*CURRENT };
    match thread.entry {
        Some(entry) => {
            interrupts::enable();
            entry();
            exit_current(Stop::Exit(0), None);
        }
        // killed before it ever ran
        None if current_killed() => exit_current(Stop::Killed, None),
        // interrupts stay off until `iretq` loads the program's flags
        None => unsafe { return_to_user((thread.stack_top() - FRAME_SIZE) as *const TrapFrame) },
    }
}

/// Loads the frame and goes to ring 3 with `iretq`
#[unsafe(naked)]
pub(crate) unsafe extern "C" fn return_to_user(frame: *const TrapFrame) -> ! {
    naked_asm!(
        "mov rsp, rdi",
        "pop r15",
        "pop r14",
        "pop r13",
        "pop r12",
        "pop r11",
        "pop r10",
        "pop r9",
        "pop r8",
        "pop rbp",
        "pop rdi",
        "pop rsi",
        "pop rdx",
        "pop rcx",
        "pop rbx",
        "pop rax",
        "add rsp, 16",
        "iretq",
    )
}

/// Takes the running thread out of the run queue until `wake`; interrupts must be off
pub fn block_current() {
    debug_assert!(!interrupts::are_enabled());
    {
        let mut guard = SCHEDULER.lock();
        let scheduler = guard.as_mut().unwrap();
        let id = scheduler.current;
        scheduler.threads.get_mut(&id).unwrap().state = State::Blocked;
    }
    schedule();
}

/// Makes a blocked thread ready again
pub fn wake(id: ThreadId) {
    let mut guard = SCHEDULER.lock();
    let Some(scheduler) = guard.as_mut() else {
        return;
    };
    if id == scheduler.idle {
        return;
    }
    if let Some(thread) = scheduler.threads.get_mut(&id).filter(|thread| thread.state == State::Blocked) {
        thread.state = State::Ready;
        scheduler.policy.add(id);
    }
}

/// `sched_yield`: the next ready thread runs, this one queues up again
pub fn yield_now() {
    schedule();
}

/// Called by the timer after its end of interrupt: switches threads every tick. A thread
/// interrupted in ring 3 of a killed process ends here instead of going back.
pub fn tick(from_user: bool) {
    if !READY.load(Ordering::SeqCst) {
        return;
    }
    end_if_killed(from_user);
    schedule();
    end_if_killed(from_user);
}

fn end_if_killed(from_user: bool) {
    let killed = from_user && unsafe { CURRENT.as_ref() }.and_then(|thread| thread.process.as_ref()).is_some_and(|p| p.is_killed());
    if killed {
        exit_current(Stop::Killed, None);
    }
}

/// Whether the running thread's process was killed: checked before going back to ring 3
pub(crate) fn current_killed() -> bool {
    unsafe { CURRENT.as_ref() }.and_then(|thread| thread.process.as_ref()).is_some_and(|p| p.is_killed())
}

/// Ends the running thread and the whole process with `stop`, `frame` and the thread's x87
/// and SSE state: other threads are woken to leave their syscalls and release locals.
/// The last thread publishes completion after shutting down child processes.
/// Used by `exit_group`, a fault, a kill and a kernel thread. Nothing may be held.
pub fn exit_current(stop: Stop, frame: Option<TrapFrame>) -> ! {
    end_current(stop, frame, true)
}

/// Ends only the running thread (`exit` of one `clone`d thread). If it was the last, the
/// process ends with `stop` like `exit_current`.
pub fn exit_thread(stop: Stop, frame: Option<TrapFrame>) -> ! {
    end_current(stop, frame, false)
}

fn end_current(stop: Stop, frame: Option<TrapFrame>, whole_process: bool) -> ! {
    interrupts::disable();
    let id = current_id();
    assert_ne!(id, BOOT_THREAD, "the boot thread can't exit");
    let process = unsafe { (*CURRENT).process.clone() };
    if let Some(process) = process {
        // this thread is leaving: the last one out, or everyone when the process ends
        let was_last = process.threads.fetch_sub(1, Ordering::SeqCst) == 1;
        if whole_process || was_last {
            if whole_process {
                process.kill();
            }
            let mut fpu_state = FpuState::initial();
            fpu::save(&mut fpu_state);
            if process.exit.lock().is_none() {
                *process.exit.lock() = Some(Exit {
                    stop,
                    frame: frame.unwrap_or_default(),
                    fpu: fpu_state,
                });
            }
        }
        if was_last {
            // This kernel has no init/reparenting service: a process owns its children.
            // Let blocked syscalls return and drop their Rust locals before stacks die.
            let children = core::mem::take(&mut *process.children.lock());
            for child in children.values() {
                child.kill();
            }
            interrupts::enable();
            for child in children.values() {
                child.wait();
            }
            drop(children);
            interrupts::disable();
            // Kernel callers retain their root descriptor table for inspection; children
            // must close theirs before notifying a parent (pipe EOF precedes wait).
            if process.parent.strong_count() != 0 {
                let files = core::mem::take(&mut *process.files.lock());
                drop(files);
            }
            process.release_vfork();
            process.completed.store(true, Ordering::SeqCst);
            let waiter = process.waiter.lock().take();
            if let Some(waiter) = waiter {
                wake(waiter);
            }
            if let Some(parent) = process.parent.upgrade() {
                for &waiter in parent.child_waiters.lock().iter() {
                    wake(waiter);
                }
            }
        }
    }
    crate::futex::forget(id);
    crate::poll::forget(id);
    crate::time::forget(id);
    crate::pipe::forget(id);
    crate::tty::forget(id);
    {
        let mut guard = SCHEDULER.lock();
        let scheduler = guard.as_mut().unwrap();
        scheduler.threads.get_mut(&id).unwrap().state = State::Dead;
    }
    schedule();
    unreachable!("a dead thread was switched to");
}

/// The trap of a fault from ring 3, for `Stop::Fault`
pub(crate) fn fault_of(frame: &TrapFrame, cr2: u64) -> Stop {
    let cr2 = if frame.vector == osel_core::trap::PAGE_FAULT { cr2 } else { 0 };
    Stop::Fault(Trap { vector: frame.vector, error_code: frame.error_code, cr2 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user::tests::{code, kernel_is_fine, load};
    use crate::user::{initial_frame, run, standard_files, start};
    use core::arch::global_asm;
    use core::sync::atomic::AtomicU64;
    use osel_core::fpu::INITIAL_MXCSR;

    /// The data page of every test program, see `user::tests::load`
    const DATA: u64 = 0x60_0000;
    /// Flags the context checker keeps: CF, PF, AF, ZF, SF, OF and IF
    const CHECKED_FLAGS: u64 = 0xad7;

    global_asm!(
        ".pushsection .rodata.osel_thread_programs, \"a\"",
        // Nikka's check_context: compares every register, the flags, XMM, MXCSR and what FS
        // points to with the values the kernel left in its data page, forever; ud2 on the
        // first difference. Every jcc is doubled, so an interrupt can land between the
        // compare and the jump.
        ".global osel_check_context", "osel_check_context:",
        "2:",
        "pushfq",
        "cmp qword ptr [rsp], 0xad7",
        "jne 3f",
        "jne 3f",
        "inc qword ptr [0x600198]",
        // xmm15 counts the rounds too: a switch that doesn't save it goes back in time
        "paddq xmm15, xmmword ptr [0x6001a0]",
        "cmp rax, qword ptr [0x600000]", "jne 3f", "jne 3f",
        "cmp rbx, qword ptr [0x600008]", "jne 3f", "jne 3f",
        "cmp rcx, qword ptr [0x600010]", "jne 3f", "jne 3f",
        "cmp rdx, qword ptr [0x600018]", "jne 3f", "jne 3f",
        "cmp rsi, qword ptr [0x600020]", "jne 3f", "jne 3f",
        "cmp rdi, qword ptr [0x600028]", "jne 3f", "jne 3f",
        "cmp rbp, qword ptr [0x600030]", "jne 3f", "jne 3f",
        "cmp r8, qword ptr [0x600038]", "jne 3f", "jne 3f",
        "cmp r9, qword ptr [0x600040]", "jne 3f", "jne 3f",
        "cmp r10, qword ptr [0x600048]", "jne 3f", "jne 3f",
        "cmp r11, qword ptr [0x600050]", "jne 3f", "jne 3f",
        "cmp r12, qword ptr [0x600058]", "jne 3f", "jne 3f",
        "cmp r13, qword ptr [0x600060]", "jne 3f", "jne 3f",
        "cmp r14, qword ptr [0x600068]", "jne 3f", "jne 3f",
        "cmp r15, qword ptr [0x600070]", "jne 3f", "jne 3f",
        "cmp rsp, qword ptr [0x600078]", "jne 3f", "jne 3f",
        "push rax",
        "movq rax, xmm0", "cmp rax, qword ptr [0x600080]", "jne 3f", "jne 3f", "movhps qword ptr [0x600190], xmm0", "mov rax, qword ptr [0x600190]", "cmp rax, qword ptr [0x600088]", "jne 3f",
        "movq rax, xmm1", "cmp rax, qword ptr [0x600090]", "jne 3f", "jne 3f", "movhps qword ptr [0x600190], xmm1", "mov rax, qword ptr [0x600190]", "cmp rax, qword ptr [0x600098]", "jne 3f",
        "movq rax, xmm2", "cmp rax, qword ptr [0x6000a0]", "jne 3f", "jne 3f", "movhps qword ptr [0x600190], xmm2", "mov rax, qword ptr [0x600190]", "cmp rax, qword ptr [0x6000a8]", "jne 3f",
        "movq rax, xmm3", "cmp rax, qword ptr [0x6000b0]", "jne 3f", "jne 3f", "movhps qword ptr [0x600190], xmm3", "mov rax, qword ptr [0x600190]", "cmp rax, qword ptr [0x6000b8]", "jne 3f",
        "movq rax, xmm4", "cmp rax, qword ptr [0x6000c0]", "jne 3f", "jne 3f", "movhps qword ptr [0x600190], xmm4", "mov rax, qword ptr [0x600190]", "cmp rax, qword ptr [0x6000c8]", "jne 3f",
        "movq rax, xmm5", "cmp rax, qword ptr [0x6000d0]", "jne 3f", "jne 3f", "movhps qword ptr [0x600190], xmm5", "mov rax, qword ptr [0x600190]", "cmp rax, qword ptr [0x6000d8]", "jne 3f",
        "movq rax, xmm6", "cmp rax, qword ptr [0x6000e0]", "jne 3f", "jne 3f", "movhps qword ptr [0x600190], xmm6", "mov rax, qword ptr [0x600190]", "cmp rax, qword ptr [0x6000e8]", "jne 3f",
        "movq rax, xmm7", "cmp rax, qword ptr [0x6000f0]", "jne 3f", "jne 3f", "movhps qword ptr [0x600190], xmm7", "mov rax, qword ptr [0x600190]", "cmp rax, qword ptr [0x6000f8]", "jne 3f",
        "movq rax, xmm8", "cmp rax, qword ptr [0x600100]", "jne 3f", "jne 3f", "movhps qword ptr [0x600190], xmm8", "mov rax, qword ptr [0x600190]", "cmp rax, qword ptr [0x600108]", "jne 3f",
        "movq rax, xmm9", "cmp rax, qword ptr [0x600110]", "jne 3f", "jne 3f", "movhps qword ptr [0x600190], xmm9", "mov rax, qword ptr [0x600190]", "cmp rax, qword ptr [0x600118]", "jne 3f",
        "movq rax, xmm10", "cmp rax, qword ptr [0x600120]", "jne 3f", "jne 3f", "movhps qword ptr [0x600190], xmm10", "mov rax, qword ptr [0x600190]", "cmp rax, qword ptr [0x600128]", "jne 3f",
        "movq rax, xmm11", "cmp rax, qword ptr [0x600130]", "jne 3f", "jne 3f", "movhps qword ptr [0x600190], xmm11", "mov rax, qword ptr [0x600190]", "cmp rax, qword ptr [0x600138]", "jne 3f",
        "movq rax, xmm12", "cmp rax, qword ptr [0x600140]", "jne 3f", "jne 3f", "movhps qword ptr [0x600190], xmm12", "mov rax, qword ptr [0x600190]", "cmp rax, qword ptr [0x600148]", "jne 3f",
        "movq rax, xmm13", "cmp rax, qword ptr [0x600150]", "jne 3f", "jne 3f", "movhps qword ptr [0x600190], xmm13", "mov rax, qword ptr [0x600190]", "cmp rax, qword ptr [0x600158]", "jne 3f",
        "movq rax, xmm14", "cmp rax, qword ptr [0x600160]", "jne 3f", "jne 3f", "movhps qword ptr [0x600190], xmm14", "mov rax, qword ptr [0x600190]", "cmp rax, qword ptr [0x600168]", "jne 3f",
        "movq rax, xmm15", "cmp rax, qword ptr [0x600198]", "jne 3f", "jne 3f", "movhps qword ptr [0x600190], xmm15", "cmp qword ptr [0x600190], 0", "jne 3f", "jne 3f",
        "stmxcsr dword ptr [0x600190]", "mov eax, dword ptr [0x600190]", "cmp eax, dword ptr [0x600180]", "jne 3f", "jne 3f",
        "mov rax, qword ptr fs:[0]", "cmp rax, qword ptr [0x600188]", "jne 3f", "jne 3f",
        "pop rax",
        "popfq",
        "jmp 2b",
        "3:", "ud2",
        ".global osel_check_context_end", "osel_check_context_end:",
        // spins forever
        ".global osel_spin_forever", "osel_spin_forever:",
        "2:",
        "jmp 2b",
        ".global osel_spin_forever_end", "osel_spin_forever_end:",
        // reads the terminal, which has nothing to give
        ".global osel_read_terminal", "osel_read_terminal:",
        "xor edi, edi",
        "mov esi, 0x600400",
        "mov edx, 16",
        "xor eax, eax",
        "syscall",
        "mov rdi, rax",
        "mov eax, 60",
        "syscall",
        ".global osel_read_terminal_end", "osel_read_terminal_end:",
        // sched_yield 100 times, then exit 0
        ".global osel_yield", "osel_yield:",
        "mov r12d, 100",
        "2:",
        "mov eax, 24",
        "syscall",
        "dec r12d",
        "jnz 2b",
        "xor edi, edi",
        "mov eax, 60",
        "syscall",
        ".global osel_yield_end", "osel_yield_end:",
        // clone a thread; the child writes 42 at 0x600200 and ends with exit (thread), the
        // parent yields until it sees it, then exits the group with that value
        ".global osel_clone", "osel_clone:",
        "mov eax, 56",
        "mov edi, 0x50f00",
        "mov esi, 0x600f00",
        "xor edx, edx",
        "xor r10d, r10d",
        "xor r8d, r8d",
        "syscall",
        "test rax, rax",
        "jz 4f",
        "mov r12, rax",             // the child's tid
        "5:",
        "mov eax, 24",
        "syscall",
        "cmp qword ptr [0x600200], 0",
        "je 5b",
        "mov rdi, qword ptr [0x600200]",
        "add rdi, r12",             // prove the parent kept the tid: 42 + tid
        "mov eax, 231",
        "syscall",
        "4:",
        "mov qword ptr [0x600200], 42",
        "xor edi, edi",
        "mov eax, 60",
        "syscall",
        ".global osel_clone_end", "osel_clone_end:",
        // clone a thread that spins forever, then the parent exits the group with rdi
        ".global osel_clone_spinner", "osel_clone_spinner:",
        "mov eax, 56",
        "mov edi, 0x50f00",
        "mov esi, 0x600f00",
        "xor edx, edx",
        "xor r10d, r10d",
        "xor r8d, r8d",
        "syscall",
        "test rax, rax",
        "jz 6f",
        "mov edi, 7",
        "mov eax, 231",
        "syscall",
        "6:",
        "jmp 6b",
        ".global osel_clone_spinner_end", "osel_clone_spinner_end:",
        // clone with bad flags (fork, not a thread): expect EINVAL, exit with -rax
        ".global osel_clone_bad", "osel_clone_bad:",
        "mov eax, 56",
        "xor edi, edi",             // flags 0: not the thread set
        "mov esi, 0x600f00",
        "syscall",
        "neg rax",
        "mov rdi, rax",
        "mov eax, 60",
        "syscall",
        ".global osel_clone_bad_end", "osel_clone_bad_end:",
        // the futex word is at 0x600200, a flag at 0x600208. The parent clones a child and
        // waits on the futex while it is 0; the child sets it to 1 and wakes one waiter,
        // then the parent exits the group with 55.
        ".global osel_futex", "osel_futex:",
        "mov eax, 56",
        "mov edi, 0x50f00",
        "mov esi, 0x600f00",
        "xor edx, edx",
        "xor r10d, r10d",
        "xor r8d, r8d",
        "syscall",
        "test rax, rax",
        "jz 7f",
        // parent: futex wait while [0x600200] == 0
        "8:",
        "cmp dword ptr [0x600200], 0",
        "jne 9f",
        "mov edi, 0x600200",             // uaddr
        "xor esi, esi",                  // FUTEX_WAIT
        "xor edx, edx",                  // expected value 0
        "xor r10d, r10d",                // no timeout
        "mov eax, 202",
        "syscall",
        "jmp 8b",
        "9:",
        "mov edi, 55",
        "mov eax, 231",
        "syscall",
        "7:",
        // child: set the word, wake one waiter, spin until reaped
        "mov dword ptr [0x600200], 1",
        "mov edi, 0x600200",
        "mov esi, 1",                    // FUTEX_WAKE
        "mov edx, 1",                    // wake one
        "mov eax, 202",
        "syscall",
        "10:", "jmp 10b",
        ".global osel_futex_end", "osel_futex_end:",
        // FUTEX_WAIT with a 30 ms timeout on a word that never changes: expect ETIMEDOUT,
        // exit with -rax
        ".global osel_futex_timeout", "osel_futex_timeout:",
        "mov dword ptr [0x600208], 123456789",
        "mov qword ptr [0x600210], 0",   // timespec: 0 seconds
        "mov qword ptr [0x600218], 30000000",  // 30 ms
        "mov edi, 0x600208",
        "xor esi, esi",                  // FUTEX_WAIT
        "mov edx, 123456789",            // the current value, so it really waits
        "mov r10d, 0x600210",            // relative timeout
        "mov eax, 202",
        "syscall",
        "neg rax",
        "mov rdi, rax",
        "mov eax, 60",
        "syscall",
        ".global osel_futex_timeout_end", "osel_futex_timeout_end:",
        ".popsection",
    );

    /// Registers, XMM, MXCSR and an FS base for one checker, all different by `seed`
    fn start_checker(seed: u64) -> Arc<Process> {
        let mut loaded = load(code!(osel_check_context, osel_check_context_end));
        let mut frame = initial_frame(loaded.entry, loaded.rsp);
        let registers: [u64; 15] = core::array::from_fn(|i| seed * 0x0101_0101_0000_0000 + i as u64 * 0x1111 + 1);
        let mut expected = [0u8; 0x1b0];
        for (i, value) in registers.iter().enumerate() {
            expected[i * 8..i * 8 + 8].copy_from_slice(&value.to_le_bytes());
        }
        (frame.rax, frame.rbx, frame.rcx, frame.rdx, frame.rsi, frame.rdi, frame.rbp) =
            (registers[0], registers[1], registers[2], registers[3], registers[4], registers[5], registers[6]);
        (frame.r8, frame.r9, frame.r10, frame.r11, frame.r12, frame.r13, frame.r14, frame.r15) =
            (registers[7], registers[8], registers[9], registers[10], registers[11], registers[12], registers[13], registers[14]);
        frame.rflags = CHECKED_FLAGS;
        expected[0x78..0x80].copy_from_slice(&(loaded.rsp - 8).to_le_bytes());
        let mut fpu = FpuState::initial();
        // xmm15 starts at 0 and counts with the rounds, by the 1 at 0x1a0
        expected[0x1a0] = 1;
        for i in 0..15 {
            let value = ((seed << 56 | i as u64) as u128) << 64 | (seed << 48 | 0xfeed_0000 | i as u64) as u128;
            fpu.0[160 + 16 * i..176 + 16 * i].copy_from_slice(&value.to_le_bytes());
            expected[0x80 + 16 * i..0x90 + 16 * i].copy_from_slice(&value.to_le_bytes());
        }
        // rounding down for one checker, up for the other: neither is the reset value
        let mxcsr = INITIAL_MXCSR | (seed as u32) << 13;
        fpu.0[24..28].copy_from_slice(&mxcsr.to_le_bytes());
        expected[0x180..0x184].copy_from_slice(&mxcsr.to_le_bytes());
        // the two checkers use FS bases at different addresses, each pointing to itself
        let fs_base = DATA + 0x800 + seed * 0x100;
        for base in [DATA + 0x900, DATA + 0xa00] {
            loaded.space.write_bytes(base, &base.to_le_bytes()).unwrap();
        }
        expected[0x188..0x190].copy_from_slice(&fs_base.to_le_bytes());
        loaded.space.write_bytes(DATA, &expected).unwrap();
        start(loaded.space, standard_files(), frame, fpu, fs_base).unwrap()
    }

    /// The counter the checker increments every round
    fn rounds(process: Arc<Process>) -> u64 {
        let (mut space, _files) = Arc::into_inner(process).expect("the process is still referenced").into_parts();
        let mut counter = [0u8; 8];
        space.read_bytes(DATA + 0x198, &mut counter).unwrap();
        u64::from_le_bytes(counter)
    }

    /// Waits with interrupts on until `done`, at most `ticks` timer ticks
    fn wait_ticks_until(ticks: u64, done: impl Fn() -> bool) {
        let start = crate::interrupts::ticks();
        while !done() && crate::interrupts::ticks() < start + ticks {
            core::hint::spin_loop();
        }
    }

    /// The design's check (Nikka `flags_and_registers_are_saved`): two threads with
    /// different patterns everywhere are switched at least 50 times without a single
    /// register going wrong, and both make progress
    #[test_case]
    fn test_two_threads_keep_their_context() {
        let (a, b) = (start_checker(1), start_checker(2));
        // the boot thread takes turns too: 150 switches give each checker 50 turns
        let before = switches();
        wait_ticks_until(2000, || {
            switches() >= before + 3 * 50 || a.has_exited() || b.has_exited()
        });
        assert!(
            !a.has_exited() && !b.has_exited(),
            "a checker found a broken register"
        );
        assert!(
            switches() >= before + 3 * 50,
            "only {} switches",
            switches() - before
        );
        a.kill();
        b.kill();
        for process in [&a, &b] {
            assert!(matches!(process.wait().stop, Stop::Killed), "a checker found a broken register");
        }
        let (a_rounds, b_rounds) = (rounds(a), rounds(b));
        assert!(a_rounds > 0 && b_rounds > 0, "both counters grow: {} and {}", a_rounds, b_rounds);
        kernel_is_fine();
    }

    /// Nikka `preemption`: a program that never gives the CPU back doesn't keep it
    #[test_case]
    fn test_an_endless_loop_is_preempted() {
        let mut loaded = load(code!(osel_spin_forever, osel_spin_forever_end));
        let frame = initial_frame(loaded.entry, loaded.rsp);
        // only the boot thread runs before: nothing to switch to
        let before = switches();
        let process = start(loaded.space.take(), standard_files(), frame, FpuState::initial(), 0).unwrap();
        // the boot thread keeps coming back while the program spins: every tick switches
        wait_ticks_until(5, || false);
        assert!(switches() >= before + 4, "the program and the boot thread did not take turns");
        process.kill();
        assert!(matches!(process.wait().stop, Stop::Killed));
        kernel_is_fine();
    }

    /// A program waiting for a key in the kernel ends when it is killed
    #[test_case]
    fn test_a_waiting_program_can_be_killed() {
        while crate::keyboard::pop_char().is_some() {}
        let mut loaded = load(code!(osel_read_terminal, osel_read_terminal_end));
        let frame = initial_frame(loaded.entry, loaded.rsp);
        let process = start(loaded.space.take(), standard_files(), frame, FpuState::initial(), 0).unwrap();
        wait_ticks_until(5, || false);
        assert!(!process.has_exited(), "the read did not wait");
        process.kill();
        assert!(matches!(process.wait().stop, Stop::Killed));
        kernel_is_fine();
    }

    #[test_case]
    fn test_sched_yield() {
        let before = switches();
        let processes: [Arc<Process>; 2] = core::array::from_fn(|_| {
            let mut loaded = load(code!(osel_yield, osel_yield_end));
            let frame = initial_frame(loaded.entry, loaded.rsp);
            start(loaded.space.take(), standard_files(), frame, FpuState::initial(), 0).unwrap()
        });
        for process in &processes {
            assert_eq!(process.wait().stop.exit_code(), 0);
        }
        assert!(switches() >= before + 200, "every yield switches: {}", switches() - before);
    }

    /// `clone` makes another thread of the same process, sharing its memory. The parent
    /// keeps the child's tid and both threads run; the whole process ends on `exit_group`.
    #[test_case]
    fn test_clone_shares_memory_and_runs() {
        let frames = crate::memory::used_frames();
        let heap = crate::allocator::with_allocator(|a| a.in_use());
        let mut loaded = load(code!(osel_clone, osel_clone_end));
        let frame = initial_frame(loaded.entry, loaded.rsp);
        let process = start(loaded.space.take(), standard_files(), frame, FpuState::initial(), 0).unwrap();
        // 42 from the child plus the child's tid the parent kept; the child is the second
        // thread made, so a small id
        let code = process.wait().stop.exit_code();
        assert!(code > 42, "parent saw the child's write and kept its tid: {code}");
        drop(Arc::into_inner(process));
        assert_eq!(crate::memory::used_frames(), frames, "both kernel stacks freed");
        assert_eq!(crate::allocator::with_allocator(|a| a.in_use()), heap);
    }

    /// `exit_group` ends every thread of the process: a spinning child is reaped, its
    /// kernel stack freed, and the kernel stays healthy.
    #[test_case]
    fn test_exit_group_reaps_a_spinning_thread() {
        let frames = crate::memory::used_frames();
        for _ in 0..20 {
            let mut loaded = load(code!(osel_clone_spinner, osel_clone_spinner_end));
            let frame = initial_frame(loaded.entry, loaded.rsp);
            let process = start(loaded.space.take(), standard_files(), frame, FpuState::initial(), 0).unwrap();
            assert_eq!(process.wait().stop.exit_code(), 7);
            drop(Arc::into_inner(process));
        }
        kernel_is_fine();
        assert_eq!(crate::memory::used_frames(), frames, "no leaked stacks");
    }

    /// One thread parks in `futex(FUTEX_WAIT)` and another wakes it with `FUTEX_WAKE`:
    /// the whole program only finishes if the wake arrived
    #[test_case]
    fn test_futex_wait_and_wake() {
        let frames = crate::memory::used_frames();
        let mut loaded = load(code!(osel_futex, osel_futex_end));
        let frame = initial_frame(loaded.entry, loaded.rsp);
        let process = start(loaded.space.take(), standard_files(), frame, FpuState::initial(), 0).unwrap();
        assert_eq!(process.wait().stop.exit_code(), 55, "the parent was woken by the child's FUTEX_WAKE");
        drop(Arc::into_inner(process));
        assert_eq!(crate::memory::used_frames(), frames);
    }

    /// `FUTEX_WAIT` with a timeout on a word that never changes ends with `ETIMEDOUT`
    #[test_case]
    fn test_futex_wait_times_out() {
        let mut loaded = load(code!(osel_futex_timeout, osel_futex_timeout_end));
        let before = crate::interrupts::ticks();
        let frame = initial_frame(loaded.entry, loaded.rsp);
        let process = start(loaded.space.take(), standard_files(), frame, FpuState::initial(), 0).unwrap();
        assert_eq!(process.wait().stop.exit_code(), Errno::ETIMEDOUT.code() as u8);
        assert!(crate::interrupts::ticks() >= before + 3, "it waited about the 30 ms timeout");
        drop(Arc::into_inner(process));
    }

    /// `clone` flags other than the thread set Go uses are `EINVAL`
    #[test_case]
    fn test_clone_rejects_other_flags() {
        let mut loaded = load(code!(osel_clone_bad, osel_clone_bad_end));
        let frame = initial_frame(loaded.entry, loaded.rsp);
        let process = start(loaded.space.take(), standard_files(), frame, FpuState::initial(), 0).unwrap();
        assert_eq!(process.wait().stop.exit_code(), Errno::EINVAL.code() as u8);
    }

    static COUNTERS: [AtomicU64; 2] = [AtomicU64::new(0), AtomicU64::new(0)];
    static STOP: AtomicBool = AtomicBool::new(false);
    static FINISHED: AtomicU64 = AtomicU64::new(0);

    fn count(index: usize) {
        while !STOP.load(Ordering::SeqCst) {
            COUNTERS[index].fetch_add(1, Ordering::Relaxed);
        }
        FINISHED.fetch_add(1, Ordering::SeqCst);
    }

    #[test_case]
    fn test_kernel_threads_take_turns() {
        STOP.store(false, Ordering::SeqCst);
        FINISHED.store(0, Ordering::SeqCst);
        let threads = [
            spawn_kernel(|| count(0)).unwrap(),
            spawn_kernel(|| count(1)).unwrap(),
        ];
        wait_ticks_until(500, || {
            COUNTERS.iter().all(|c| c.load(Ordering::Relaxed) > 0)
        });
        assert!(
            COUNTERS.iter().all(|c| c.load(Ordering::Relaxed) > 0),
            "a thread never ran"
        );
        STOP.store(true, Ordering::SeqCst);
        wait_ticks_until(500, || FINISHED.load(Ordering::SeqCst) == 2);
        assert_eq!(FINISHED.load(Ordering::SeqCst), 2);
        // a thread is freed by whoever runs after it, once it really ended
        wait_ticks_until(500, || threads.iter().all(|&id| runs(id).is_none()));
        assert!(threads.iter().all(|&id| runs(id).is_none()), "a finished thread is still there");
    }

    /// Threads and their stacks come and go without leaking frames or heap
    #[test_case]
    fn test_many_threads_leak_nothing() {
        let frames = memory::used_frames();
        let heap = crate::allocator::with_allocator(|allocator| allocator.in_use());
        for _ in 0..100 {
            let mut loaded = load(code!(osel_yield, osel_yield_end));
            let mut frame = initial_frame(loaded.entry, loaded.rsp);
            assert_eq!(run(&mut loaded.space, &mut frame).exit_code(), 0);
        }
        assert_eq!(memory::used_frames(), frames);
        assert_eq!(crate::allocator::with_allocator(|allocator| allocator.in_use()), heap);
    }

    #[test_case]
    fn test_kernel_stacks_have_unmapped_guards() {
        use x86_64::structures::paging::Translate;
        let slot = STACK_SLOTS.lock().as_ref().unwrap().slot(0);
        let translate = |address: u64| memory::with_memory(|m| m.mapper.translate_addr(VirtAddr::new(address)));
        assert!(translate(slot.guard).is_none(), "the guard page is mapped");
        assert!(translate(slot.bottom).is_some() && translate(slot.top - 8).is_some(), "the idle thread's stack");
        assert_eq!(STACK_SLOTS.lock().as_ref().unwrap().guard_of(slot.guard + 100), Some(0));
    }
}
