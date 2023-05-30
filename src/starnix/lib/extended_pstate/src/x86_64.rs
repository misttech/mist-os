// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use once_cell::sync::Lazy;
use static_assertions::const_assert_eq;

pub struct State {
    buffer: XSaveArea,
    strategy: Strategy,
}

#[derive(Default)]
#[repr(C)]
struct X87MMXState {
    low: u64,
    high: u64,
}

#[derive(Default)]
#[repr(C)]
struct SSERegister {
    low: u64,
    high: u64,
}

// [intel/vol1] Table 10-2. Format of an FXSAVE Area
#[repr(C)]
struct X86LegacySaveArea {
    fcw: u16,
    fsw: u16,
    ftw: u8,
    _reserved: u8,

    fop: u16,
    fip: u64,
    fdp: u64,

    mxcsr: u32,
    mxcsr_mask: u32,

    st: [X87MMXState; 8],

    xmm: [SSERegister; 16],
}

const_assert_eq!(std::mem::size_of::<X86LegacySaveArea>(), 416);

#[repr(C, align(16))]
struct FXSaveArea {
    x86_legacy_save_area: X86LegacySaveArea,
    _reserved: [u8; 96],
}
const_assert_eq!(std::mem::size_of::<FXSaveArea>(), 512);

impl Default for FXSaveArea {
    fn default() -> Self {
        Self {
            x86_legacy_save_area: X86LegacySaveArea {
                fcw: 0x37f, // All exceptions masked, no exceptions raised.
                fsw: 0,
                // The ftw field stores an abbreviated version where all zero bits match the default.
                // See [intel/vol1] 10.5.1.1 x87 State for details.
                ftw: 0,
                _reserved: Default::default(),
                fop: 0,
                fip: 0,
                fdp: 0,
                mxcsr: 0x3f << 7, // All exceptions masked, no exceptions raised.
                mxcsr_mask: 0,
                st: Default::default(),
                xmm: Default::default(),
            },
            _reserved: [0; 96],
        }
    }
}

#[repr(C, align(64))]
struct XSaveArea {
    fxsave_area: FXSaveArea,
    xsave_header: [u8; 64],
    // High 128 bits of ymm0-15 registers
    avx_state: [u8; 256],
    // TODO: Size of the extended region is dynamic depending on which features are enabled.
    // See [intel/vol1] 13.5 XSAVE-MANAGED STATE
}

const_assert_eq!(std::mem::size_of::<XSaveArea>(), 832);

impl XSaveArea {
    fn addr(&self) -> *const u8 {
        self as *const _ as *const u8
    }

    fn addr_mut(&mut self) -> *mut u8 {
        self as *mut _ as *mut u8
    }
}

impl Default for XSaveArea {
    fn default() -> Self {
        Self { fxsave_area: Default::default(), xsave_header: [0; 64], avx_state: [0; 256] }
    }
}

#[derive(PartialEq, Debug, Copy, Clone, PartialOrd)]
pub enum Strategy {
    XSaveOpt,
    XSave,
    FXSave,
}

pub static PREFERRED_STRATEGY: Lazy<Strategy> = Lazy::new(|| {
    if is_x86_feature_detected!("xsaveopt") {
        Strategy::XSaveOpt
    } else if is_x86_feature_detected!("xsave") {
        Strategy::XSave
    } else {
        // The FXSave strategy does not preserve the high 128 bits of the YMM
        // register. If we find hardware that requires this, we need to add
        // support for saving and restoring these through load/store
        // instructions with the VEX.256 prefix and remove this assertion.
        // [intel/vol1]: 14.8 ACCESSING YMM REGISTERS
        assert!(!is_x86_feature_detected!("avx"));
        Strategy::FXSave
    }
});

impl State {
    pub fn with_strategy(strategy: Strategy) -> Self {
        Self { buffer: XSaveArea::default(), strategy }
    }

    #[inline(always)]
    pub(crate) fn save(&mut self) {
        match self.strategy {
            Strategy::XSaveOpt => unsafe {
                std::arch::x86_64::_xsaveopt(self.buffer.addr_mut(), u64::MAX);
            },
            Strategy::XSave => unsafe {
                std::arch::x86_64::_xsave(self.buffer.addr_mut(), u64::MAX);
            },
            Strategy::FXSave => unsafe {
                std::arch::x86_64::_fxsave(self.buffer.addr_mut());
            },
        }
    }

    #[inline(always)]
    // Safety: See comment in lib.rs.
    pub(crate) unsafe fn restore(&self) {
        match self.strategy {
            Strategy::XSave | Strategy::XSaveOpt => {
                std::arch::x86_64::_xrstor(self.buffer.addr(), u64::MAX)
            }
            Strategy::FXSave => std::arch::x86_64::_fxrstor(self.buffer.addr()),
        }
    }

    pub fn reset(&mut self) {
        self.initialize_saved_area()
    }

    fn initialize_saved_area(&mut self) {
        *self = Default::default()
    }
}

impl Default for State {
    fn default() -> Self {
        Self { buffer: XSaveArea::default(), strategy: *PREFERRED_STRATEGY }
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[::fuchsia::test]
    fn save_restore_sse_registers() {
        use core::arch::asm;

        let write_custom_state = || {
            // x87 FPU status word
            //   x87 FPU Status Word: FSTSW/FNSTSW, FSTENV/FNSTENV
            // Exception state lives in the status word
            // The exception flags are “sticky” bits (once set, they remain set until explicitly cleared). They can be cleared by
            // executing the FCLEX/FNCLEX (clear exceptions) instructions, by reinitializing the x87 FPU with the FINIT/FNINIT or
            // FSAVE/FNSAVE instructions, or by overwriting the flags with an FRSTOR or FLDENV instruction.

            // We expect the FPU stack to be empty. Pop a value to generate a stack underflow exception
            let flt = [0u8; 8];
            unsafe {
                asm!("fstp dword ptr [{flt}]", flt = in(reg) &flt as *const u8);
            }
            // Check that the IE and SF bits are 1 and the C1 flag is 0. [intel/vol1] 8.5.1.1 Stack Overflow or Underflow Exception (#IS)
            let fpust = 0u16;
            unsafe {
                asm!("fnstsw [{fpust}]", fpust = in(reg)&fpust);
            }
            assert_eq!(fpust & 1 << 0, 0x1); // IE flag, bit 0
            assert_eq!(fpust & 1 << 6, 1 << 6); // SF flag, bit 6
            assert_eq!(fpust & 1 << 9, 0); // C1 flag, bit 9.

            // x87 FPU control word.
            let mut fpucw = 0u16;
            unsafe {
                asm!("fnstcw [{fpucw}]", fpucw = in(reg) &fpucw);
            }
            // Unmask all 6 x87 exceptions
            fpucw &= !0x3f;
            unsafe {
                asm!("fldcw [{fpucw}]", fpucw = in(reg) &fpucw);
            }

            let mut mxcsr = 0u32;
            unsafe {
                asm!("stmxcsr [{mxcsr}]", mxcsr = in(reg) &mxcsr);
            }
            // Unmask the lowest 3 exceptions.
            mxcsr &= !(0x7 << 7);
            unsafe {
                asm!("ldmxcsr [{mxcsr}]", mxcsr = in(reg) &mxcsr);
            }

            // Populate SSE registers
            let vals_a = [0x42u8; 16];
            let vals_b = [0x43u8; 16];
            let vals_c = [0x44u8; 16];
            unsafe {
                asm!("movups xmm0, [{vals_a}]
                          movups xmm1, [{vals_b}]
                          movups xmm2, [{vals_c}]",
                    vals_a = in(reg) &vals_a,
                    vals_b = in(reg) &vals_b,
                    vals_c = in(reg) &vals_c,
                    out("xmm0") _,
                    out("xmm1") _,
                    out("xmm2") _,
                );
            }
        };

        let clear_state = || {
            unsafe {
                // Reinitialize x87 FPU
                asm!("fninit");
                // Reset SSE control state to all exceptions masked, no exceptions detected
                let mxcsr = 0x3f << 7;
                asm!("ldmxcsr [{mxcsr}]", mxcsr = in(reg) &mxcsr);
                // Clear SSE registers
                asm!("xorps xmm0, xmm0
                          xorps xmm1, xmm1
                          xorps xmm2, xmm2",
                    out("xmm0") _,
                    out("xmm1") _,
                    out("xmm2") _,
                );
            }
        };

        let dest = [0u8; 16];
        let validate_state_cleared = || {
            let fpust = 0u16;
            unsafe {
                asm!("fnstsw [{fpust}]", fpust = in(reg)&fpust);
            }
            assert_eq!(fpust, 0);

            let fpucw = 0u16;
            unsafe { asm!("fnstcw [{fpucw}]", fpucw = in(reg) &fpucw) };
            assert_eq!(fpucw, 0x37f); // Initial FPU state per [intel/vol1] 8.1.5 x87 FPU Control Word

            let mxcsr = 0u32;
            unsafe {
                asm!("stmxcsr [{mxcsr}]", mxcsr = in(reg) &mxcsr);
            }
            assert_eq!(mxcsr & 0x1f, 0); // No exceptions raised.
            assert_eq!((mxcsr >> 7) & 0x3f, 0x3f); // All exceptions masked.
            unsafe {
                asm!("movups [{dest}], xmm0", dest = in(reg) &dest);
            }
            for i in 0..16 {
                assert_eq!(dest[i], 0);
            }
            unsafe {
                asm!("movups [{dest}], xmm1", dest = in(reg) &dest);
            }
            for i in 0..16 {
                assert_eq!(dest[i], 0);
            }
            unsafe {
                asm!("movups [{dest}], xmm2", dest = in(reg) &dest);
            }
            for i in 0..16 {
                assert_eq!(dest[i], 0);
            }
        };

        let validate_state_restored = || {
            // x87 FPU status word

            // Check that the IE and SF bits are 1 and the C1 flag is 0. [intel/vol1] 8.5.1.1 Stack Overflow or Underflow Exception (#IS)
            let fpust = 0u16;
            unsafe {
                asm!("fnstsw [{fpust}]", fpust = in(reg)&fpust);
            }
            assert_eq!(fpust & 1 << 0, 0x1); // IE flag, bit 0
            assert_eq!(fpust & 1 << 6, 1 << 6); // SF flag, bit 6
            assert_eq!(fpust & 1 << 9, 0); // C1 flag, bit 9.

            // x87 FPU control word
            let fpucw = 0u16;
            unsafe { asm!("fnstcw [{fpucw}]", fpucw = in(reg) &fpucw) };
            assert_eq!(fpucw, 0x340); // All exceptions masked, 64 bit precision, round to nearest.

            let mxcsr = 0u32;
            unsafe {
                asm!("stmxcsr [{mxcsr}]", mxcsr = in(reg) &mxcsr);
            }
            assert_eq!(mxcsr & 0x1f, 0); // No exceptions raised.
            assert_eq!((mxcsr >> 7) & 0x3f, 0x38); // First 3 exceptions unmasked, rest masked.

            // SSE registers
            unsafe {
                asm!("movups [{dest}], xmm0", dest = in(reg) &dest);
            }
            for i in 0..16 {
                assert_eq!(dest[i], 0x42);
            }
            unsafe {
                asm!("movups [{dest}], xmm1", dest = in(reg) &dest);
            }
            for i in 0..16 {
                assert_eq!(dest[i], 0x43);
            }
            unsafe {
                asm!("movups [{dest}], xmm2", dest = in(reg) &dest);
            }
            for i in 0..16 {
                assert_eq!(dest[i], 0x44);
            }
        };

        let mut state = State::default();
        write_custom_state();
        state.save();
        clear_state();
        validate_state_cleared();
        unsafe {
            state.restore();
        }
        validate_state_restored();
    }
}
