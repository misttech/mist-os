// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::arch::asm;
use static_assertions::const_assert_eq;

const NUM_FP_REGISTERS: usize = 32;
const NUM_V_REGISTERS: usize = 32;

// Currently only VLEN=128 is supported.
const VLEN: usize = 128;

#[derive(Copy, Clone, Default, PartialEq, Debug)]
#[repr(C)]
pub struct RiscvVectorCsrs {
    pub vcsr: u64,
    pub vstart: u64,
    pub vl: u64,
    pub vtype: u64,
}

// Helpers for V extension, see `riscv64_vector.S`. We cannot use inline asm for these yet because
// support for V extension in Rust is not stable yet.
extern "C" {
    fn get_riscv64_vlenb() -> usize;

    #[allow(improper_ctypes)]
    fn save_riscv64_v_registers(v_registers: *mut u128, vcsrs: &mut RiscvVectorCsrs);

    #[allow(improper_ctypes)]
    fn restore_riscv64_v_registers(v_registers: *const u128, vcsrs: &RiscvVectorCsrs);
}

#[derive(Clone, Copy, Default)]
#[repr(C)]
pub struct State {
    // Floating-point registers from the F and D extensions.
    pub fp_registers: [u64; NUM_FP_REGISTERS],
    pub fcsr: u32,

    // V registers size depends on the CPU and is not defined in compile time. Allocate the buffer
    // for these registers on the heap.
    pub v_registers: [u128; NUM_V_REGISTERS],
    pub vcsrs: RiscvVectorCsrs,
}

const_assert_eq!(std::mem::align_of::<u128>(), VLEN / 8);
const_assert_eq!(std::mem::size_of::<u128>(), VLEN / 8);

impl State {
    #[inline(always)]
    pub(crate) fn save(&mut self) {
        unsafe {
            if get_riscv64_vlenb() != VLEN / 8 {
                panic!("Only VLEN={} is supported", VLEN);
            }

            asm!(
              "fsd  f0,  0 * 8({regs})",
              "fsd  f1,  1 * 8({regs})",
              "fsd  f2,  2 * 8({regs})",
              "fsd  f3,  3 * 8({regs})",
              "fsd  f4,  4 * 8({regs})",
              "fsd  f5,  5 * 8({regs})",
              "fsd  f6,  6 * 8({regs})",
              "fsd  f7,  7 * 8({regs})",
              "fsd  f8,  8 * 8({regs})",
              "fsd  f9,  9 * 8({regs})",
              "fsd f10, 10 * 8({regs})",
              "fsd f11, 11 * 8({regs})",
              "fsd f12, 12 * 8({regs})",
              "fsd f13, 13 * 8({regs})",
              "fsd f14, 14 * 8({regs})",
              "fsd f15, 15 * 8({regs})",
              "fsd f16, 16 * 8({regs})",
              "fsd f17, 17 * 8({regs})",
              "fsd f18, 18 * 8({regs})",
              "fsd f19, 19 * 8({regs})",
              "fsd f20, 20 * 8({regs})",
              "fsd f21, 21 * 8({regs})",
              "fsd f22, 22 * 8({regs})",
              "fsd f23, 23 * 8({regs})",
              "fsd f24, 24 * 8({regs})",
              "fsd f25, 25 * 8({regs})",
              "fsd f26, 26 * 8({regs})",
              "fsd f27, 27 * 8({regs})",
              "fsd f28, 28 * 8({regs})",
              "fsd f29, 29 * 8({regs})",
              "fsd f30, 30 * 8({regs})",
              "fsd f31, 31 * 8({regs})",
              regs = in(reg) &self.fp_registers,
            );
            asm!(
              "csrr {fcsr}, fcsr",
              fcsr = out(reg) self.fcsr,
            );

            save_riscv64_v_registers(self.v_registers.as_mut_ptr(), &mut self.vcsrs);
        }
    }

    #[inline(always)]
    // Safety: See comment in lib.rs.
    pub(crate) unsafe fn restore(&self) {
        asm!(
            "fld  f0,  0 * 8({regs})",
            "fld  f1,  1 * 8({regs})",
            "fld  f2,  2 * 8({regs})",
            "fld  f3,  3 * 8({regs})",
            "fld  f4,  4 * 8({regs})",
            "fld  f5,  5 * 8({regs})",
            "fld  f6,  6 * 8({regs})",
            "fld  f7,  7 * 8({regs})",
            "fld  f8,  8 * 8({regs})",
            "fld  f9,  9 * 8({regs})",
            "fld f10, 10 * 8({regs})",
            "fld f11, 11 * 8({regs})",
            "fld f12, 12 * 8({regs})",
            "fld f13, 13 * 8({regs})",
            "fld f14, 14 * 8({regs})",
            "fld f15, 15 * 8({regs})",
            "fld f16, 16 * 8({regs})",
            "fld f17, 17 * 8({regs})",
            "fld f18, 18 * 8({regs})",
            "fld f19, 19 * 8({regs})",
            "fld f20, 20 * 8({regs})",
            "fld f21, 21 * 8({regs})",
            "fld f22, 22 * 8({regs})",
            "fld f23, 23 * 8({regs})",
            "fld f24, 24 * 8({regs})",
            "fld f25, 25 * 8({regs})",
            "fld f26, 26 * 8({regs})",
            "fld f27, 27 * 8({regs})",
            "fld f28, 28 * 8({regs})",
            "fld f29, 29 * 8({regs})",
            "fld f30, 30 * 8({regs})",
            "fld f31, 31 * 8({regs})",
            regs = in(reg) &self.fp_registers,
        );
        asm!(
            "fscsr {fcsr}",
            fcsr = in(reg) self.fcsr,
        );

        restore_riscv64_v_registers(self.v_registers.as_ptr(), &self.vcsrs);
    }

    pub fn reset(&mut self) {
        *self = Default::default();
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[::fuchsia::test]
    fn save_restore_registers() {
        use core::arch::asm;

        let mut state = State::default();

        let f0 = 6.41352134f64;
        let f10 = 10.5134f64;
        let f31 = -153.5754f64;

        // Set rounding mode to 0o111.
        let fcsr: u32 = 0x000000e0;

        // Set custom state by hand.
        unsafe {
            asm!("fmv.d.x f0, {f0}", f0 = in(reg) f0);
            asm!("fmv.d.x f10, {f10}", f10 = in(reg) f10);
            asm!("fmv.d.x f31, {f31}", f31 = in(reg) f31);
            asm!("fscsr {fcsr}", fcsr = in(reg) fcsr);
        }

        state.save();

        // Clear state manually.
        unsafe {
            asm!("fmv.d.x f0, zero");
            asm!("fmv.d.x f10, zero");
            asm!("fmv.d.x f31, zero");
            asm!("fscsr zero");
        }

        // Verify that the state is cleared.
        {
            let f0: f64;
            let f10: f64;
            let f31: f64;
            let fcsr: u64;
            unsafe {
                asm!("fmv.x.d {f0}, f0", f0 = out(reg) f0);
                asm!("fmv.x.d {f10}, f10", f10 = out(reg) f10);
                asm!("fmv.x.d {f31}, f31", f31 = out(reg) f31);
                asm!("frcsr {fcsr}", fcsr = out(reg) fcsr);
            }

            assert_eq!(f0, 0.0f64);
            assert_eq!(f10, 0.0f64);
            assert_eq!(f31, 0.0f64);
            assert_eq!(fcsr, 0);
        }

        unsafe {
            state.restore();
        }

        // Verify that the state restored to what we expect.
        {
            let f0_restored: f64;
            let f10_restored: f64;
            let f31_restored: f64;
            let fcsr_restored: u32;
            unsafe {
                asm!("fmv.x.d {f0}, f0", f0 = out(reg) f0_restored);
                asm!("fmv.x.d {f10}, f10", f10 = out(reg) f10_restored);
                asm!("fmv.x.d {f31}, f31", f31 = out(reg) f31_restored);
                asm!("frcsr {fcsr}", fcsr = out(reg) fcsr_restored);
            }
            assert_eq!(f0, f0_restored);
            assert_eq!(f10, f10_restored);
            assert_eq!(f31, f31_restored);
            assert_eq!(fcsr, fcsr_restored);
        }
    }

    #[::fuchsia::test]
    fn save_restore_v_registers() {
        let mut state = State::default();

        // Get default state.
        state.save();

        for i in 0..state.v_registers.len() {
            state.v_registers[i] = 0x123456789abcdef << i;
        }

        // This calls `memset()` which may mutate V registers, so it should be done before the
        // `restore_riscv64_v_registers()` call below.
        let mut state2 = State::default();

        unsafe {
            state.restore();
        }
        state2.save();

        assert_eq!(state.v_registers, state2.v_registers);
        assert_eq!(state.vcsrs, state2.vcsrs);
    }
}
