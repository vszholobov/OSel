/// CPU interrupts module
use crate::gdt;
use crate::keyboard;
use crate::sync::IrqSpinlock;
use core::sync::atomic::{AtomicU64, Ordering};
use lazy_static::lazy_static;
use osel_core::exception::{Details, ExceptionReport, Frame};
use osel_core::pit;
use pic8259::ChainedPics;
use x86_64::instructions::port::Port;
use x86_64::registers::control::Cr2;
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

/// Hardware interrupts are remapped right after the 32 CPU exception vectors
pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

static PICS: IrqSpinlock<ChainedPics> =
    IrqSpinlock::new("PICS", unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

/// Runs `f` with the PICs locked (and interrupts off)
pub fn with_pics<R>(f: impl FnOnce(&mut ChainedPics) -> R) -> R {
    f(&mut PICS.lock())
}

#[derive(Debug, Clone, Copy)]
#[repr(u8)]
pub enum InterruptIndex {
    Timer = PIC_1_OFFSET,
    Keyboard,
    /// IRQ 7, the master PIC reports spurious interrupts on it
    SpuriousMaster = PIC_1_OFFSET + 7,
    /// IRQ 15, the slave PIC reports spurious interrupts on it
    SpuriousSlave = PIC_2_OFFSET + 7,
}

/// Timer interrupts per second
pub const TICK_HZ: u32 = 100;

impl InterruptIndex {
    fn as_u8(self) -> u8 {
        self as u8
    }
}

/// Entry stub of an exception that a program can cause. From ring 0 it jumps to the
/// kernel's handler with the stack untouched, so kernel exceptions behave as before. From
/// ring 3 it pushes the vector (and a zero for a missing error code) and goes to
/// `user::user_trap`, which ends the program's run.
macro_rules! entry_stub {
    ($stub:ident, $vector:literal, $handler:ident: HandlerFunc) => {
        const _: x86_64::structures::idt::HandlerFunc = $handler;
        #[unsafe(naked)]
        unsafe extern "C" fn $stub() {
            core::arch::naked_asm!(
                // CS the CPU pushed: ring 0 or 3
                "test qword ptr [rsp + 8], 3",
                "jz {handler}",
                "push 0",
                "push {vector}",
                "jmp {user_trap}",
                handler = sym $handler,
                vector = const $vector,
                user_trap = sym crate::user::user_trap,
            )
        }
    };
    ($stub:ident, $vector:literal, $handler:ident: $kind:ident) => {
        const _: x86_64::structures::idt::$kind = $handler;
        #[unsafe(naked)]
        unsafe extern "C" fn $stub() {
            core::arch::naked_asm!(
                // the error code sits below the CPU frame
                "test qword ptr [rsp + 16], 3",
                "jz {handler}",
                "push {vector}",
                "jmp {user_trap}",
                handler = sym $handler,
                vector = const $vector,
                user_trap = sym crate::user::user_trap,
            )
        }
    };
}

entry_stub!(divide_error_stub, 0, divide_error_handler: HandlerFunc);
entry_stub!(debug_stub, 1, debug_handler: HandlerFunc);
entry_stub!(breakpoint_stub, 3, breakpoint_handler: HandlerFunc);
entry_stub!(overflow_stub, 4, overflow_handler: HandlerFunc);
entry_stub!(bound_range_exceeded_stub, 5, bound_range_exceeded_handler: HandlerFunc);
entry_stub!(invalid_opcode_stub, 6, invalid_opcode_handler: HandlerFunc);
entry_stub!(device_not_available_stub, 7, device_not_available_handler: HandlerFunc);
entry_stub!(invalid_tss_stub, 10, invalid_tss_handler: HandlerFuncWithErrCode);
entry_stub!(segment_not_present_stub, 11, segment_not_present_handler: HandlerFuncWithErrCode);
entry_stub!(stack_segment_fault_stub, 12, stack_segment_fault_handler: HandlerFuncWithErrCode);
entry_stub!(general_protection_fault_stub, 13, general_protection_fault_handler: HandlerFuncWithErrCode);
entry_stub!(page_fault_stub, 14, page_fault_handler: PageFaultHandlerFunc);
entry_stub!(x87_floating_point_stub, 16, x87_floating_point_handler: HandlerFunc);
entry_stub!(alignment_check_stub, 17, alignment_check_handler: HandlerFuncWithErrCode);
entry_stub!(simd_floating_point_stub, 19, simd_floating_point_handler: HandlerFunc);
entry_stub!(virtualization_stub, 20, virtualization_handler: HandlerFunc);
entry_stub!(control_protection_stub, 21, control_protection_handler: HandlerFuncWithErrCode);
entry_stub!(hv_injection_exception_stub, 28, hv_injection_exception_handler: HandlerFunc);
entry_stub!(vmm_communication_exception_stub, 29, vmm_communication_exception_handler: HandlerFuncWithErrCode);
entry_stub!(security_exception_stub, 30, security_exception_handler: HandlerFuncWithErrCode);

/// Points `entry` at `stub` after `set_handler_fn` checked the handler's type. Resets the
/// entry's options: set a stack index or a privilege level after this, not before.
unsafe fn through_stub<F>(entry: &mut x86_64::structures::idt::Entry<F>, stub: unsafe extern "C" fn()) {
    unsafe { entry.set_handler_addr(x86_64::VirtAddr::new(stub as usize as u64)) };
}

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.debug.set_handler_fn(debug_handler);
        // NMI and machine check can arrive at any moment, even with a broken stack
        unsafe {
            idt.non_maskable_interrupt
                .set_handler_fn(non_maskable_interrupt_handler)
                .set_stack_index(gdt::NMI_IST_INDEX);
            idt.machine_check.set_handler_fn(machine_check_handler).set_stack_index(gdt::MACHINE_CHECK_IST_INDEX);
        }
        idt.overflow.set_handler_fn(overflow_handler);
        idt.bound_range_exceeded.set_handler_fn(bound_range_exceeded_handler);
        idt.device_not_available.set_handler_fn(device_not_available_handler);
        idt.invalid_tss.set_handler_fn(invalid_tss_handler);
        idt.x87_floating_point.set_handler_fn(x87_floating_point_handler);
        idt.alignment_check.set_handler_fn(alignment_check_handler);
        idt.simd_floating_point.set_handler_fn(simd_floating_point_handler);
        idt.virtualization.set_handler_fn(virtualization_handler);
        idt.cp_protection_exception.set_handler_fn(control_protection_handler);
        idt.security_exception.set_handler_fn(security_exception_handler);
        idt.hv_injection_exception.set_handler_fn(hv_injection_exception_handler);
        idt.vmm_communication_exception.set_handler_fn(vmm_communication_exception_handler);
        idt.divide_error.set_handler_fn(divide_error_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.segment_not_present.set_handler_fn(segment_not_present_handler);
        idt.stack_segment_fault.set_handler_fn(stack_segment_fault_handler);
        idt.general_protection_fault.set_handler_fn(general_protection_fault_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
        // the exceptions a program can cause enter through stubs that tell ring 3 from ring 0
        unsafe {
            through_stub(&mut idt.divide_error, divide_error_stub);
            through_stub(&mut idt.debug, debug_stub);
            through_stub(&mut idt.breakpoint, breakpoint_stub);
            through_stub(&mut idt.overflow, overflow_stub);
            through_stub(&mut idt.bound_range_exceeded, bound_range_exceeded_stub);
            through_stub(&mut idt.invalid_opcode, invalid_opcode_stub);
            through_stub(&mut idt.device_not_available, device_not_available_stub);
            through_stub(&mut idt.invalid_tss, invalid_tss_stub);
            through_stub(&mut idt.segment_not_present, segment_not_present_stub);
            through_stub(&mut idt.stack_segment_fault, stack_segment_fault_stub);
            through_stub(&mut idt.general_protection_fault, general_protection_fault_stub);
            through_stub(&mut idt.page_fault, page_fault_stub);
            through_stub(&mut idt.x87_floating_point, x87_floating_point_stub);
            through_stub(&mut idt.alignment_check, alignment_check_stub);
            through_stub(&mut idt.simd_floating_point, simd_floating_point_stub);
            through_stub(&mut idt.virtualization, virtualization_stub);
            through_stub(&mut idt.cp_protection_exception, control_protection_stub);
            through_stub(&mut idt.hv_injection_exception, hv_injection_exception_stub);
            through_stub(&mut idt.vmm_communication_exception, vmm_communication_exception_stub);
            through_stub(&mut idt.security_exception, security_exception_stub);
        }
        unsafe {
            idt.double_fault.set_handler_fn(double_fault_handler)
                .set_stack_index(gdt::DOUBLE_FAULT_IST_INDEX); // new
        }
        idt[InterruptIndex::Timer.as_u8()].set_handler_fn(timer_interrupt_handler);
        idt[InterruptIndex::Keyboard.as_u8()].set_handler_fn(keyboard_interrupt_handler);
        idt[InterruptIndex::SpuriousMaster.as_u8()].set_handler_fn(spurious_master_handler);
        idt[InterruptIndex::SpuriousSlave.as_u8()].set_handler_fn(spurious_slave_handler);
        idt
    };
}

pub fn init_idt() {
    IDT.load();
}

/// Lines of the legacy PICs the kernel handles
const ENABLED_IRQS: [u8; 2] = [0, 1];

/// Remaps the PICs, programs the timer to `TICK_HZ` and unmasks the timer (IRQ 0)
/// and keyboard (IRQ 1) lines. UEFI firmware leaves every line masked and the PIT
/// at its own rate, so both have to be set explicitly.
pub fn init_pics() {
    let (master_mask, slave_mask) = pit::pic_masks(&ENABLED_IRQS);
    let divisor = pit::divisor(TICK_HZ);
    let mut pics = PICS.lock();
    // a byte left in the PS/2 controller (say, a key release from the boot menu) keeps
    // IRQ 1 high; the PIC reset below then never sees a new edge and the keyboard
    // stays silent forever
    flush_keyboard_controller();
    unsafe {
        pics.initialize();
        // channel 0, low then high byte, rate generator
        Port::<u8>::new(0x43).write(0b0011_0100);
        Port::<u8>::new(0x40).write(divisor as u8);
        Port::<u8>::new(0x40).write((divisor >> 8) as u8);
        pics.write_masks(master_mask, slave_mask);
    }
}

/// Drops whatever waits in the PS/2 controller output buffer
fn flush_keyboard_controller() {
    let mut status = Port::<u8>::new(0x64);
    let mut data = Port::<u8>::new(0x60);
    // bounded: a broken controller may report a full buffer forever
    for _ in 0..64 {
        if unsafe { status.read() } & 1 == 0 {
            break;
        }
        let _: u8 = unsafe { data.read() };
    }
}

static TICKS: AtomicU64 = AtomicU64::new(0);

/// Number of timer interrupts since boot, `TICK_HZ` per second
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Reads one CMOS register through the index/data ports
fn cmos(register: u8) -> u8 {
    let mut index = Port::<u8>::new(0x70);
    let mut data = Port::<u8>::new(0x71);
    unsafe {
        index.write(register);
        data.read()
    }
}

/// The real-time clock as nanoseconds since the Unix epoch, read once at boot. Handles the
/// BCD and 12-hour encodings the CMOS status register reports (QEMU uses BCD, 24-hour).
pub fn wall_clock_unix_nanos() -> u64 {
    // read twice and accept once two reads agree: an update may start mid-read
    let mut last = read_cmos_datetime();
    loop {
        let again = read_cmos_datetime();
        if again == last {
            return osel_core::clock::unix_nanos(again);
        }
        last = again;
    }
}

fn read_cmos_datetime() -> osel_core::clock::DateTime {
    // wait out an update so the fields are consistent
    while cmos(0x0a) & 0x80 != 0 {}
    let status_b = cmos(0x0b);
    let binary = status_b & 0x04 != 0;
    let hour_24 = status_b & 0x02 != 0;
    let decode = |value: u8| {
        if binary {
            value
        } else {
            (value >> 4) * 10 + (value & 0x0f)
        }
    };
    let second = decode(cmos(0x00));
    let minute = decode(cmos(0x02));
    let raw_hour = cmos(0x04);
    let mut hour = decode(raw_hour & 0x7f);
    if !hour_24 && raw_hour & 0x80 != 0 {
        hour = (hour % 12) + 12;
    } else if !hour_24 && hour == 12 {
        hour = 0;
    }
    let day = decode(cmos(0x07));
    let month = decode(cmos(0x08));
    let year = 2000 + decode(cmos(0x09)) as u16;
    osel_core::clock::DateTime { year, month, day, hour, minute, second }
}

static BREAKPOINTS: AtomicU64 = AtomicU64::new(0);
static SPURIOUS: AtomicU64 = AtomicU64::new(0);

/// Number of `int3` breakpoints hit since boot
pub fn breakpoints() -> u64 {
    BREAKPOINTS.load(Ordering::Relaxed)
}

/// Number of spurious PIC interrupts since boot
pub fn spurious_interrupts() -> u64 {
    SPURIOUS.load(Ordering::Relaxed)
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    BREAKPOINTS.fetch_add(1, Ordering::Relaxed);
    // a breakpoint can hit code that holds the console lock: skip printing then
    crate::avganec::try_print(format_args!("EXCEPTION: BREAKPOINT at {:#x}\n", stack_frame.instruction_pointer.as_u64()));
}

extern "x86-interrupt" fn double_fault_handler(stack_frame: InterruptStackFrame, error_code: u64) -> ! {
    fatal_exception("DOUBLE FAULT", &stack_frame, Details::ErrorCode(error_code));
}

extern "x86-interrupt" fn debug_handler(stack_frame: InterruptStackFrame) {
    fatal_exception("DEBUG", &stack_frame, Details::None);
}

extern "x86-interrupt" fn non_maskable_interrupt_handler(stack_frame: InterruptStackFrame) {
    fatal_exception("NON-MASKABLE INTERRUPT", &stack_frame, Details::None);
}

extern "x86-interrupt" fn overflow_handler(stack_frame: InterruptStackFrame) {
    fatal_exception("OVERFLOW", &stack_frame, Details::None);
}

extern "x86-interrupt" fn bound_range_exceeded_handler(stack_frame: InterruptStackFrame) {
    fatal_exception("BOUND RANGE EXCEEDED", &stack_frame, Details::None);
}

extern "x86-interrupt" fn device_not_available_handler(stack_frame: InterruptStackFrame) {
    fatal_exception("DEVICE NOT AVAILABLE", &stack_frame, Details::None);
}

extern "x86-interrupt" fn invalid_tss_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    fatal_exception("INVALID TSS", &stack_frame, Details::Selector(error_code));
}

extern "x86-interrupt" fn x87_floating_point_handler(stack_frame: InterruptStackFrame) {
    fatal_exception("X87 FLOATING POINT", &stack_frame, Details::None);
}

extern "x86-interrupt" fn alignment_check_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    fatal_exception("ALIGNMENT CHECK", &stack_frame, Details::ErrorCode(error_code));
}

extern "x86-interrupt" fn machine_check_handler(stack_frame: InterruptStackFrame) -> ! {
    fatal_exception("MACHINE CHECK", &stack_frame, Details::None);
}

extern "x86-interrupt" fn simd_floating_point_handler(stack_frame: InterruptStackFrame) {
    fatal_exception("SIMD FLOATING POINT", &stack_frame, Details::None);
}

extern "x86-interrupt" fn virtualization_handler(stack_frame: InterruptStackFrame) {
    fatal_exception("VIRTUALIZATION", &stack_frame, Details::None);
}

extern "x86-interrupt" fn control_protection_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    fatal_exception("CONTROL PROTECTION", &stack_frame, Details::ErrorCode(error_code));
}

extern "x86-interrupt" fn security_exception_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    fatal_exception("SECURITY EXCEPTION", &stack_frame, Details::ErrorCode(error_code));
}

extern "x86-interrupt" fn hv_injection_exception_handler(stack_frame: InterruptStackFrame) {
    fatal_exception("HYPERVISOR INJECTION EXCEPTION", &stack_frame, Details::None);
}

extern "x86-interrupt" fn vmm_communication_exception_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    fatal_exception("VMM COMMUNICATION EXCEPTION", &stack_frame, Details::ErrorCode(error_code));
}

extern "x86-interrupt" fn divide_error_handler(stack_frame: InterruptStackFrame) {
    fatal_exception("DIVIDE ERROR", &stack_frame, Details::None);
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    fatal_exception("INVALID OPCODE", &stack_frame, Details::None);
}

extern "x86-interrupt" fn segment_not_present_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    fatal_exception("SEGMENT NOT PRESENT", &stack_frame, Details::Selector(error_code));
}

extern "x86-interrupt" fn stack_segment_fault_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    fatal_exception("STACK SEGMENT FAULT", &stack_frame, Details::Selector(error_code));
}

extern "x86-interrupt" fn general_protection_fault_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    fatal_exception("GENERAL PROTECTION FAULT", &stack_frame, Details::Selector(error_code));
}

extern "x86-interrupt" fn page_fault_handler(stack_frame: InterruptStackFrame, error_code: PageFaultErrorCode) {
    let details = Details::PageFault { code: error_code.bits(), address: Cr2::read_raw() };
    fatal_exception("PAGE FAULT", &stack_frame, details);
}

/// Panics with the CPU state saved by the exception, the panic handler shows it on screen
#[track_caller]
fn fatal_exception(name: &str, frame: &InterruptStackFrame, details: Details) -> ! {
    let frame = Frame {
        rip: frame.instruction_pointer.as_u64(),
        rsp: frame.stack_pointer.as_u64(),
        cs: frame.code_segment.0,
        ss: frame.stack_segment.0,
        rflags: frame.cpu_flags.bits(),
    };
    panic!("{}", ExceptionReport { name, frame, details });
}

extern "x86-interrupt" fn timer_interrupt_handler(stack_frame: InterruptStackFrame) {
    TICKS.fetch_add(1, Ordering::Relaxed);
    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
    // wake sleepers whose deadline passed, then let the next thread run
    crate::time::wake_due();
    // after the end of interrupt and with no lock held: the next thread may run for a while
    crate::tty::poll_serial();
    crate::scheduler::tick(stack_frame.code_segment.rpl() == x86_64::PrivilegeLevel::Ring3);
}

/// Reads the in-service register of the PIC at `command_port`
fn in_service_register(command_port: u16) -> u8 {
    let mut port = Port::<u8>::new(command_port);
    unsafe {
        port.write(0x0b);
        port.read()
    }
}

extern "x86-interrupt" fn spurious_master_handler(_stack_frame: InterruptStackFrame) {
    if pit::is_spurious(in_service_register(0x20), 7) {
        // nothing is in service, so there is nothing to acknowledge
        SPURIOUS.fetch_add(1, Ordering::Relaxed);
        return;
    }
    unsafe { PICS.lock().notify_end_of_interrupt(InterruptIndex::SpuriousMaster.as_u8()) };
}

extern "x86-interrupt" fn spurious_slave_handler(_stack_frame: InterruptStackFrame) {
    if pit::is_spurious(in_service_register(0xa0), 15) {
        SPURIOUS.fetch_add(1, Ordering::Relaxed);
        // the master did see the cascade line, it still needs its acknowledgement
        unsafe { Port::<u8>::new(0x20).write(0x20) };
        return;
    }
    unsafe { PICS.lock().notify_end_of_interrupt(InterruptIndex::SpuriousSlave.as_u8()) };
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let mut port = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };
    keyboard::handle_scancode(scancode);
    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test_case]
    fn test_breakpoint_exception_returns() {
        let before = breakpoints();
        x86_64::instructions::interrupts::int3();
        x86_64::instructions::interrupts::int3();
        assert_eq!(breakpoints(), before + 2);
    }

    fn wait_ticks(count: u64) {
        let start = ticks();
        let mut halts = 0;
        while ticks() < start + count {
            x86_64::instructions::hlt();
            halts += 1;
            assert!(halts < 1000 * count, "timer does not tick");
        }
    }

    #[test_case]
    fn test_timer_ticks() {
        wait_ticks(2);
    }

    #[test_case]
    fn test_pic_masks() {
        // the timer handler takes the same lock
        let masks = with_pics(|pics| unsafe { pics.read_masks() });
        assert_eq!(masks, [0b1111_1100, 0b1111_1111]);
    }

    #[test_case]
    fn test_interrupts_are_enabled() {
        assert!(x86_64::instructions::interrupts::are_enabled());
    }

    #[test_case]
    fn test_spurious_master_interrupt_is_ignored() {
        let before = spurious_interrupts();
        // a software interrupt on the IRQ 7 vector: nothing is in service, like a real spurious one
        unsafe { core::arch::asm!("int {}", const PIC_1_OFFSET + 7) };
        assert_eq!(spurious_interrupts(), before + 1);
        wait_ticks(2);
    }

    #[test_case]
    fn test_spurious_slave_interrupt_is_ignored() {
        let before = spurious_interrupts();
        unsafe { core::arch::asm!("int {}", const PIC_2_OFFSET + 7) };
        assert_eq!(spurious_interrupts(), before + 1);
        // the master saw the cascade line and got its acknowledgement: nothing in service
        assert_eq!(in_service_register(0x20), 0);
        wait_ticks(2);
    }

    /// Seconds of the real time clock in the CMOS
    fn rtc_seconds() -> u8 {
        let mut index = Port::<u8>::new(0x70);
        let mut data = Port::<u8>::new(0x71);
        unsafe {
            // wait for an update in progress to finish
            loop {
                index.write(0x0a);
                if data.read() & 0x80 == 0 {
                    break;
                }
            }
            index.write(0x00);
            data.read()
        }
    }

    #[test_case]
    fn test_timer_frequency_against_rtc() {
        // count ticks during one full RTC second
        let wait_next_second = || {
            let second = rtc_seconds();
            let start = ticks();
            while rtc_seconds() == second {
                assert!(ticks() < start + 5 * TICK_HZ as u64, "RTC is not running");
                x86_64::instructions::hlt();
            }
        };
        wait_next_second();
        let start = ticks();
        wait_next_second();
        wait_next_second();
        let per_second = (ticks() - start) / 2;
        // QEMU without hardware acceleration drops ticks on a busy host, but 18.2 Hz
        // (an unprogrammed PIT) and 1 kHz must still be caught
        assert!((25..=400).contains(&per_second), "{} ticks per second, expected {}", per_second, TICK_HZ);
    }
}
