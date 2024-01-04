// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::arch::asm;
use static_assertions::const_assert_eq;

#[derive(Clone, Copy, Default)]
pub struct State {
    // Floating-point registers from the F and D extensions.
    pub fp_registers: [u64; 32usize],
    pub fcsr: u32,
    // TODO(https://fxbug.dev/124336): Save state for the V extension if necessary.
}

const_assert_eq!(std::mem::size_of::<State>(), 264);

impl State {
    #[inline(always)]
    pub(crate) fn save(&mut self) {
        unsafe {
            asm!(
              "fsd  f0,  0 * 8({state})",
              "fsd  f1,  1 * 8({state})",
              "fsd  f2,  2 * 8({state})",
              "fsd  f3,  3 * 8({state})",
              "fsd  f4,  4 * 8({state})",
              "fsd  f5,  5 * 8({state})",
              "fsd  f6,  6 * 8({state})",
              "fsd  f7,  7 * 8({state})",
              "fsd  f8,  8 * 8({state})",
              "fsd  f9,  9 * 8({state})",
              "fsd f10, 10 * 8({state})",
              "fsd f11, 11 * 8({state})",
              "fsd f12, 12 * 8({state})",
              "fsd f13, 13 * 8({state})",
              "fsd f14, 14 * 8({state})",
              "fsd f15, 15 * 8({state})",
              "fsd f16, 16 * 8({state})",
              "fsd f17, 17 * 8({state})",
              "fsd f18, 18 * 8({state})",
              "fsd f19, 19 * 8({state})",
              "fsd f20, 20 * 8({state})",
              "fsd f21, 21 * 8({state})",
              "fsd f22, 22 * 8({state})",
              "fsd f23, 23 * 8({state})",
              "fsd f24, 24 * 8({state})",
              "fsd f25, 25 * 8({state})",
              "fsd f26, 26 * 8({state})",
              "fsd f27, 27 * 8({state})",
              "fsd f28, 28 * 8({state})",
              "fsd f29, 29 * 8({state})",
              "fsd f30, 30 * 8({state})",
              "fsd f31, 31 * 8({state})",
              "frcsr {fcsr}",
              state = in(reg) &self.fp_registers,
              fcsr = out(reg) self.fcsr,
            );
        }
    }

    #[inline(always)]
    // Safety: See comment in lib.rs.
    pub(crate) unsafe fn restore(&self) {
        asm!(
            "fld  f0,  0 * 8({state})",
            "fld  f1,  1 * 8({state})",
            "fld  f2,  2 * 8({state})",
            "fld  f3,  3 * 8({state})",
            "fld  f4,  4 * 8({state})",
            "fld  f5,  5 * 8({state})",
            "fld  f6,  6 * 8({state})",
            "fld  f7,  7 * 8({state})",
            "fld  f8,  8 * 8({state})",
            "fld  f9,  9 * 8({state})",
            "fld f10, 10 * 8({state})",
            "fld f11, 11 * 8({state})",
            "fld f12, 12 * 8({state})",
            "fld f13, 13 * 8({state})",
            "fld f14, 14 * 8({state})",
            "fld f15, 15 * 8({state})",
            "fld f16, 16 * 8({state})",
            "fld f17, 17 * 8({state})",
            "fld f18, 18 * 8({state})",
            "fld f19, 19 * 8({state})",
            "fld f20, 20 * 8({state})",
            "fld f21, 21 * 8({state})",
            "fld f22, 22 * 8({state})",
            "fld f23, 23 * 8({state})",
            "fld f24, 24 * 8({state})",
            "fld f25, 25 * 8({state})",
            "fld f26, 26 * 8({state})",
            "fld f27, 27 * 8({state})",
            "fld f28, 28 * 8({state})",
            "fld f29, 29 * 8({state})",
            "fld f30, 30 * 8({state})",
            "fld f31, 31 * 8({state})",
            "fscsr {fcsr}",
            state = in(reg) &self.fp_registers,
            fcsr = in(reg) self.fcsr,
        );
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
}
