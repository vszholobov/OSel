/// CPU interrupts module
use crate::gdt;
use crate::keyboard;
use crate::println;
use core::sync::atomic::{AtomicU64, Ordering};
use osel_core::pit;
use lazy_static::lazy_static;
use pic8259::ChainedPics;
use spin::Mutex;
use x86_64::instructions::port::Port;
use x86_64::registers::control::Cr2;
use osel_core::exception::{Details, ExceptionReport, Frame};
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

/// Hardware interrupts are remapped right after the 32 CPU exception vectors
pub const PIC_1_OFFSET: u8 = 32;
pub const PIC_2_OFFSET: u8 = PIC_1_OFFSET + 8;

pub static PICS: Mutex<ChainedPics> = Mutex::new(unsafe { ChainedPics::new(PIC_1_OFFSET, PIC_2_OFFSET) });

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

lazy_static! {
    static ref IDT: InterruptDescriptorTable = {
        let mut idt = InterruptDescriptorTable::new();
        idt.breakpoint.set_handler_fn(breakpoint_handler);
        idt.divide_error.set_handler_fn(divide_error_handler);
        idt.invalid_opcode.set_handler_fn(invalid_opcode_handler);
        idt.segment_not_present.set_handler_fn(segment_not_present_handler);
        idt.stack_segment_fault.set_handler_fn(stack_segment_fault_handler);
        idt.general_protection_fault.set_handler_fn(general_protection_fault_handler);
        idt.page_fault.set_handler_fn(page_fault_handler);
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
    unsafe {
        pics.initialize();
        // channel 0, low then high byte, rate generator
        Port::<u8>::new(0x43).write(0b0011_0100);
        Port::<u8>::new(0x40).write(divisor as u8);
        Port::<u8>::new(0x40).write((divisor >> 8) as u8);
        pics.write_masks(master_mask, slave_mask);
    }
}

static TICKS: AtomicU64 = AtomicU64::new(0);

/// Number of timer interrupts since boot, `TICK_HZ` per second
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
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
    println!("EXCEPTION: BREAKPOINT at {:#x}", stack_frame.instruction_pointer.as_u64());
}

extern "x86-interrupt" fn double_fault_handler(stack_frame: InterruptStackFrame, error_code: u64) -> ! {
    fatal_exception("DOUBLE FAULT", &stack_frame, Details::ErrorCode(error_code));
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

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    TICKS.fetch_add(1, Ordering::Relaxed);
    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
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
        let masks = unsafe { PICS.lock().read_masks() };
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
        let per_second = ticks() - start;
        // QEMU without hardware acceleration is not exact, but 18.2 Hz or 1 kHz must be caught
        assert!((50..=200).contains(&per_second), "{} ticks per second, expected {}", per_second, TICK_HZ);
    }
}
