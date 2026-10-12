//! fpu module turns on SSE and x87 for programs and moves their state in and out
//!
//! The kernel itself is built soft-float and never touches these registers, so a program's
//! state only has to be loaded before it runs and saved after. The layout of the state is
//! `osel_core::fpu::FpuState`.

use osel_core::fpu::FpuState;
use x86_64::registers::control::{Cr0, Cr0Flags, Cr4, Cr4Flags};

/// Lets programs use x87 and SSE: no emulation, native x87 error reporting (#MF), the
/// state saved by `fxsave`, unmasked SIMD exceptions as #XM. The bootloader sets none of
/// this under BIOS; UEFI firmware hands over with most of it on.
pub fn init() {
    unsafe {
        Cr0::update(|cr0| {
            cr0.remove(Cr0Flags::EMULATE_COPROCESSOR | Cr0Flags::TASK_SWITCHED);
            cr0.insert(Cr0Flags::MONITOR_COPROCESSOR | Cr0Flags::NUMERIC_ERROR);
        });
        Cr4::update(|cr4| cr4.insert(Cr4Flags::OSFXSR | Cr4Flags::OSXMMEXCPT_ENABLE));
    }
}

/// Loads a program's x87 and SSE registers. Inline assembly should leave the x87 state
/// alone; here changing it is the point, and the soft-float kernel never uses it.
pub fn restore(state: &FpuState) {
    unsafe { core::arch::asm!("fxrstor64 [{}]", in(reg) state, options(nostack, readonly, preserves_flags)) };
}

/// Saves a program's x87 and SSE registers
pub fn save(state: &mut FpuState) {
    unsafe { core::arch::asm!("fxsave64 [{}]", in(reg) state, options(nostack, preserves_flags)) };
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::user::tests::{code, kernel_is_fine, load};
    use crate::user::{Stop, initial_frame, run_with_fpu};
    use core::arch::global_asm;
    use osel_core::trap::TrapFrame;

    global_asm!(
        ".pushsection .rodata.osel_fpu_programs, \"a\"",
        // every XMM register gets a pattern (SSE2 only: QEMU's default CPU has no SSE4.1),
        // one x87 register is pushed, then a fault
        "osel_xmm_patterns:",
        "mov rax, 0x101010101010101", "movq xmm0, rax", "punpcklqdq xmm0, xmm0",
        "mov rax, 0x202020202020202", "movq xmm1, rax", "punpcklqdq xmm1, xmm1",
        "mov rax, 0x303030303030303", "movq xmm2, rax", "punpcklqdq xmm2, xmm2",
        "mov rax, 0x404040404040404", "movq xmm3, rax", "punpcklqdq xmm3, xmm3",
        "mov rax, 0x505050505050505", "movq xmm4, rax", "punpcklqdq xmm4, xmm4",
        "mov rax, 0x606060606060606", "movq xmm5, rax", "punpcklqdq xmm5, xmm5",
        "mov rax, 0x707070707070707", "movq xmm6, rax", "punpcklqdq xmm6, xmm6",
        "mov rax, 0x808080808080808", "movq xmm7, rax", "punpcklqdq xmm7, xmm7",
        "mov rax, 0x909090909090909", "movq xmm8, rax", "punpcklqdq xmm8, xmm8",
        "mov rax, 0xa0a0a0a0a0a0a0a", "movq xmm9, rax", "punpcklqdq xmm9, xmm9",
        "mov rax, 0xb0b0b0b0b0b0b0b", "movq xmm10, rax", "punpcklqdq xmm10, xmm10",
        "mov rax, 0xc0c0c0c0c0c0c0c", "movq xmm11, rax", "punpcklqdq xmm11, xmm11",
        "mov rax, 0xd0d0d0d0d0d0d0d", "movq xmm12, rax", "punpcklqdq xmm12, xmm12",
        "mov rax, 0xe0e0e0e0e0e0e0e", "movq xmm13, rax", "punpcklqdq xmm13, xmm13",
        "mov rax, 0xf0f0f0f0f0f0f0f", "movq xmm14, rax", "punpcklqdq xmm14, xmm14",
        "mov rax, 0x1010101010101010", "movq xmm15, rax", "punpcklqdq xmm15, xmm15",
        "fld1",
        "mov qword ptr [0], rax",
        "osel_xmm_patterns_end:",
        // what a new program finds: MXCSR in rax, FCW in rbx, xmm5 in rcx, FSW in rdx
        "osel_fresh_state:",
        "sub rsp, 16",
        "stmxcsr [rsp]",
        "mov eax, [rsp]",
        "fnstcw [rsp + 4]",
        "movzx ebx, word ptr [rsp + 4]",
        "fnstsw [rsp + 8]",
        "movzx edx, word ptr [rsp + 8]",
        "movq rcx, xmm5",
        "mov qword ptr [0], rax",
        "osel_fresh_state_end:",
        // SSE division by zero with the exception unmasked
        "osel_simd_divide:",
        "sub rsp, 8",
        "stmxcsr [rsp]",
        "and dword ptr [rsp], 0xfffffdff",
        "ldmxcsr [rsp]",
        "mov eax, 1",
        "cvtsi2ss xmm0, eax",
        "xorps xmm1, xmm1",
        "osel_simd_divide_at:",
        "divss xmm0, xmm1",
        "osel_simd_divide_next:",
        "mov qword ptr [0], rax",
        "osel_simd_divide_end:",
        // x87 division by zero with the exception unmasked: reported at the next fwait
        "osel_x87_divide:",
        "sub rsp, 8",
        "fnstcw [rsp]",
        "and word ptr [rsp], 0xfffb",
        "fldcw [rsp]",
        "fld1",
        "fldz",
        "fdivp",
        "osel_x87_divide_wait:",
        "fwait",
        "osel_x87_divide_end:",
        // the same, but a system call between the division and fwait: the pending
        // exception must reach the program, not the kernel
        "osel_x87_pending:",
        "sub rsp, 8",
        "fnstcw [rsp]",
        "and word ptr [rsp], 0xfffb",
        "fldcw [rsp]",
        "fld1",
        "fldz",
        "fdivp",
        "mov eax, 39",
        "syscall",
        "fwait",
        "osel_x87_pending_end:",
        // xmm3 survives a system call and timer interrupts
        "osel_xmm_kept:",
        "mov rax, 0x3333333333333333",
        "movq xmm3, rax",
        "punpcklqdq xmm3, xmm3",
        "mov eax, 39",
        "syscall",
        "mov ecx, 200000000",
        "2:",
        "dec ecx",
        "jnz 2b",
        "mov qword ptr [0], rax",
        "osel_xmm_kept_end:",
        ".popsection",
    );

    fn run_code(code: &[u8], fpu: &mut FpuState) -> (TrapFrame, Stop) {
        let mut loaded = load(code);
        let mut frame = initial_frame(loaded.entry, loaded.rsp);
        let stop = run_with_fpu(&mut loaded.space, &mut frame, fpu);
        (frame, stop)
    }

    #[test_case]
    fn test_a_program_keeps_its_xmm_registers() {
        let mut fpu = FpuState::initial();
        let (_frame, stop) = run_code(code!(osel_xmm_patterns, osel_xmm_patterns_end), &mut fpu);
        assert_eq!(stop.fault().vector, 14);
        for n in 0..16 {
            let half = (n as u128 + 1) * 0x0101_0101_0101_0101;
            assert_eq!(fpu.xmm(n), half << 64 | half, "xmm{}", n);
        }
        // 1.0 as an 80-bit float: exponent 0x3fff, explicit integer bit
        assert_eq!(fpu.st(0), [0, 0, 0, 0, 0, 0, 0, 0x80, 0xff, 0x3f], "fld1 is on top of the x87 stack");
        kernel_is_fine();
    }

    /// Nothing of one program's x87 and SSE state reaches the next one
    #[test_case]
    fn test_a_new_program_starts_with_reset_registers() {
        let (_frame, stop) = run_code(code!(osel_xmm_patterns, osel_xmm_patterns_end), &mut FpuState::initial());
        assert_eq!(stop.fault().vector, 14);
        // through `run`, as every program starts
        let (frame, stop, _loaded) = crate::user::tests::run_program(code!(osel_fresh_state, osel_fresh_state_end));
        assert_eq!(stop.fault().vector, 14);
        kernel_is_fine();
        assert_eq!(frame.rax, 0x1f80, "MXCSR");
        assert_eq!(frame.rbx, 0x037f, "FCW");
        assert_eq!(frame.rcx, 0, "xmm5 of the previous program");
        assert_eq!(frame.rdx, 0, "FSW: an empty x87 stack");
    }

    #[test_case]
    fn test_an_unmasked_simd_exception_ends_the_program() {
        let mut fpu = FpuState::initial();
        let (frame, stop) = run_code(code!(osel_simd_divide, osel_simd_divide_end), &mut fpu);
        let at = |label: &[u8]| crate::user::tests::TEXT + label.len() as u64;
        // real CPUs raise #XM at divss; QEMU without KVM only sets the flag and goes on
        let trap = stop.fault();
        match trap.vector {
            19 => assert_eq!(frame.rip, at(code!(osel_simd_divide, osel_simd_divide_at))),
            14 => {
                assert_eq!(frame.rip, at(code!(osel_simd_divide, osel_simd_divide_next)), "divss ran to the end");
                assert_eq!(trap.cr2, 0);
            }
            vector => panic!("vector {}: SSE is not on", vector),
        }
        assert!(fpu.mxcsr() & 0b100 != 0, "the divide by zero flag");
        assert_eq!(fpu.mxcsr() & 0x200, 0, "the program unmasked it");
        kernel_is_fine();
    }

    #[test_case]
    fn test_an_unmasked_x87_exception_ends_the_program() {
        let wait = code!(osel_x87_divide, osel_x87_divide_wait).len() as u64;
        let (frame, stop) = run_code(code!(osel_x87_divide, osel_x87_divide_end), &mut FpuState::initial());
        assert_eq!(stop.fault().vector, 16, "#MF");
        assert_eq!(frame.rip, crate::user::tests::TEXT + wait, "reported at fwait");
        kernel_is_fine();
    }

    #[test_case]
    fn test_a_pending_x87_exception_waits_for_the_program() {
        let (_frame, stop) = run_code(code!(osel_x87_pending, osel_x87_pending_end), &mut FpuState::initial());
        assert_eq!(stop.fault().vector, 16, "#MF in the program, after the system call");
        kernel_is_fine();
    }

    #[test_case]
    fn test_xmm_survives_calls_and_interrupts() {
        let mut fpu = FpuState::initial();
        let before = crate::interrupts::ticks();
        let (_frame, stop) = run_code(code!(osel_xmm_kept, osel_xmm_kept_end), &mut fpu);
        assert_eq!(stop.fault().vector, 14);
        assert!(crate::interrupts::ticks() > before, "no interrupt while the program ran");
        assert_eq!(fpu.xmm(3), 0x3333_3333_3333_3333_3333_3333_3333_3333);
        kernel_is_fine();
    }

    #[test_case]
    fn test_sse_is_on_and_avx_is_off() {
        let cr0 = Cr0::read();
        assert!(!cr0.contains(Cr0Flags::EMULATE_COPROCESSOR));
        assert!(!cr0.contains(Cr0Flags::TASK_SWITCHED));
        assert!(cr0.contains(Cr0Flags::MONITOR_COPROCESSOR | Cr0Flags::NUMERIC_ERROR));
        let cr4 = Cr4::read();
        assert!(cr4.contains(Cr4Flags::OSFXSR | Cr4Flags::OSXMMEXCPT_ENABLE));
        assert!(!cr4.contains(Cr4Flags::OSXSAVE), "no XSAVE: AVX stays off, Go with GOAMD64=v1 needs SSE2 only");
    }

    #[test_case]
    fn test_save_gives_back_what_restore_loaded() {
        let mut state = FpuState::initial();
        state.0[160..176].copy_from_slice(&0x0123_4567_89ab_cdef_fedc_ba98_7654_3210u128.to_le_bytes());
        restore(&state);
        let mut saved = FpuState([0xff; 512]);
        save(&mut saved);
        assert_eq!(saved.xmm(0), 0x0123_4567_89ab_cdef_fedc_ba98_7654_3210);
        assert_eq!(saved.fcw(), 0x037f);
        assert_eq!(saved.mxcsr(), 0x1f80);
    }
}
