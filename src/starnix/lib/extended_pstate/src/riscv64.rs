// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::arch::asm;
use static_assertions::const_assert_eq;

#[derive(Default)]
pub struct State {
    // Floating-point registers from the F and D extensions.
    pub fp_registers: [u64; 32usize],
    pub fcsr: u32,
    // TODO(fxbug.dev/124336): Save state for the V extension if necessary.
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

            // `fscsr` swaps the `fcsr` register value with the specified register, so it needs
            // to be declared as `inout()`.
            fcsr = inout(reg) self.fcsr => _,
        );
    }

    pub fn reset(&mut self) {
        *self = Default::default();
    }
}

// TODO(fxbug.dev/128554): Add tests.
