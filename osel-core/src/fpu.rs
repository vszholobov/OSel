//! The x87 and SSE state of a program, in the layout of `fxsave64`

/// x87 control word after `fninit`: all exceptions masked, 64-bit precision, round to nearest
pub const INITIAL_FCW: u16 = 0x037f;
/// MXCSR after reset: all SIMD exceptions masked, round to nearest
pub const INITIAL_MXCSR: u32 = 0x1f80;

const FCW: usize = 0;
const MXCSR: usize = 24;
const ST: usize = 32;
const XMM: usize = 160;

/// The 512 byte area `fxsave64` writes and `fxrstor64` reads, 16 byte aligned
#[repr(C, align(16))]
#[derive(Clone, PartialEq, Eq)]
pub struct FpuState(pub [u8; 512]);

impl FpuState {
    /// What a new program starts with: the registers after `fninit` and a reset MXCSR,
    /// every x87 and XMM register empty and zero
    pub fn initial() -> FpuState {
        let mut state = FpuState([0; 512]);
        state.0[FCW..FCW + 2].copy_from_slice(&INITIAL_FCW.to_le_bytes());
        state.0[MXCSR..MXCSR + 4].copy_from_slice(&INITIAL_MXCSR.to_le_bytes());
        state
    }

    pub fn fcw(&self) -> u16 {
        u16::from_le_bytes(self.0[FCW..FCW + 2].try_into().unwrap())
    }

    pub fn mxcsr(&self) -> u32 {
        u32::from_le_bytes(self.0[MXCSR..MXCSR + 4].try_into().unwrap())
    }

    /// XMM register `n` (0..16)
    pub fn xmm(&self, n: usize) -> u128 {
        let at = XMM + 16 * n;
        u128::from_le_bytes(self.0[at..at + 16].try_into().unwrap())
    }

    /// The 80-bit x87 register ST(`n`) (0..8), counted from the top of the stack as
    /// `fxsave` stores them, in the low 10 of its 16 bytes
    pub fn st(&self, n: usize) -> [u8; 10] {
        let at = ST + 16 * n;
        self.0[at..at + 10].try_into().unwrap()
    }
}

impl core::fmt::Debug for FpuState {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        write!(f, "FpuState {{ fcw: {:#x}, mxcsr: {:#x}, xmm0: {:#x} }}", self.fcw(), self.mxcsr(), self.xmm(0))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layout_is_fxsave64() {
        assert_eq!(core::mem::size_of::<FpuState>(), 512);
        assert_eq!(core::mem::align_of::<FpuState>(), 16, "fxsave faults on an unaligned area");
    }

    #[test]
    fn a_new_program_starts_clean() {
        let state = FpuState::initial();
        assert_eq!(state.fcw(), 0x037f);
        assert_eq!(state.mxcsr(), 0x1f80);
        assert!((0..16).all(|n| state.xmm(n) == 0));
        assert!((0..8).all(|n| state.st(n) == [0; 10]));
        // the abridged tag word: 0 means every x87 register is empty
        assert_eq!(state.0[4], 0);
    }

    #[test]
    fn registers_are_read_where_fxsave_puts_them() {
        let mut state = FpuState::initial();
        state.0[160 + 16 * 3..160 + 16 * 4].copy_from_slice(&0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00u128.to_le_bytes());
        assert_eq!(state.xmm(3), 0x1122_3344_5566_7788_99aa_bbcc_ddee_ff00);
        assert_eq!(state.xmm(2), 0);
    }
}
