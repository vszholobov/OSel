/// CPU interrupts module
use crate::gdt;
use crate::keyboard;
use crate::println;
use core::sync::atomic::{AtomicU64, Ordering};
use lazy_static::lazy_static;
use pic8259::ChainedPics;
use spin::Mutex;
use x86_64::instructions::port::Port;
use x86_64::registers::control::Cr2;
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
}

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
        idt
    };
}

pub fn init_idt() {
    IDT.load();
}

/// Remaps the PICs and unmasks the timer (IRQ 0) and keyboard (IRQ 1) lines.
/// UEFI firmware leaves every line masked, so they have to be opened explicitly.
pub fn init_pics() {
    let mut pics = PICS.lock();
    unsafe {
        pics.initialize();
        pics.write_masks(0b1111_1100, 0b1111_1111);
    }
}

static TICKS: AtomicU64 = AtomicU64::new(0);

/// Number of timer interrupts since boot (the PIT fires ~18.2 times per second by default)
pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

extern "x86-interrupt" fn breakpoint_handler(stack_frame: InterruptStackFrame) {
    println!("EXCEPTION: BREAKPOINT\n{:#?}", stack_frame);
}

extern "x86-interrupt" fn double_fault_handler(
    stack_frame: InterruptStackFrame,
    _error_code: u64,
) -> ! {
    fatal_exception("DOUBLE FAULT", &stack_frame, None, format_args!(""));
}

extern "x86-interrupt" fn divide_error_handler(stack_frame: InterruptStackFrame) {
    fatal_exception("DIVIDE ERROR", &stack_frame, None, format_args!(""));
}

extern "x86-interrupt" fn invalid_opcode_handler(stack_frame: InterruptStackFrame) {
    fatal_exception("INVALID OPCODE", &stack_frame, None, format_args!(""));
}

extern "x86-interrupt" fn segment_not_present_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    fatal_exception("SEGMENT NOT PRESENT", &stack_frame, Some(error_code), format_args!(""));
}

extern "x86-interrupt" fn stack_segment_fault_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    fatal_exception("STACK SEGMENT FAULT", &stack_frame, Some(error_code), format_args!(""));
}

extern "x86-interrupt" fn general_protection_fault_handler(stack_frame: InterruptStackFrame, error_code: u64) {
    fatal_exception("GENERAL PROTECTION FAULT", &stack_frame, Some(error_code), format_args!(""));
}

extern "x86-interrupt" fn page_fault_handler(stack_frame: InterruptStackFrame, error_code: PageFaultErrorCode) {
    let address = Cr2::read().map_or(0, |address| address.as_u64());
    let cause = if error_code.contains(PageFaultErrorCode::PROTECTION_VIOLATION) {
        "protection violation"
    } else {
        "page not present"
    };
    let access = if error_code.contains(PageFaultErrorCode::INSTRUCTION_FETCH) {
        "instruction fetch"
    } else if error_code.contains(PageFaultErrorCode::CAUSED_BY_WRITE) {
        "write"
    } else {
        "read"
    };
    let mode = if error_code.contains(PageFaultErrorCode::USER_MODE) { "user" } else { "kernel" };
    fatal_exception(
        "PAGE FAULT",
        &stack_frame,
        Some(error_code.bits()),
        format_args!("\n  accessed address: {:#x}\n  reason: {} on {} by {}", address, cause, access, mode),
    );
}

/// Panics with the CPU state saved by the exception, the panic handler shows it on screen
#[track_caller]
fn fatal_exception(name: &str, frame: &InterruptStackFrame, error_code: Option<u64>, details: core::fmt::Arguments) -> ! {
    struct ErrorCode(Option<u64>);
    impl core::fmt::Display for ErrorCode {
        fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
            match self.0 {
                Some(code) => write!(f, "\n  error code: {:#x}", code),
                None => Ok(()),
            }
        }
    }
    panic!(
        "EXCEPTION: {}\n  rip: {:#018x}  rsp: {:#018x}\n  cs: {:#06x}  ss: {:#06x}  rflags: {:#x}{}{}",
        name,
        frame.instruction_pointer.as_u64(),
        frame.stack_pointer.as_u64(),
        frame.code_segment.0,
        frame.stack_segment.0,
        frame.cpu_flags.bits(),
        ErrorCode(error_code),
        details,
    );
}

extern "x86-interrupt" fn timer_interrupt_handler(_stack_frame: InterruptStackFrame) {
    TICKS.fetch_add(1, Ordering::Relaxed);
    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Timer.as_u8());
    }
}

extern "x86-interrupt" fn keyboard_interrupt_handler(_stack_frame: InterruptStackFrame) {
    let mut port = Port::new(0x60);
    let scancode: u8 = unsafe { port.read() };
    keyboard::handle_scancode(scancode);
    unsafe {
        PICS.lock().notify_end_of_interrupt(InterruptIndex::Keyboard.as_u8());
    }
}

#[test_case]
fn test_breakpoint_exception() {
    x86_64::instructions::interrupts::int3();
}

#[test_case]
fn test_timer_ticks() {
    let start = ticks();
    while ticks() < start + 2 {
        x86_64::instructions::hlt();
    }
}
