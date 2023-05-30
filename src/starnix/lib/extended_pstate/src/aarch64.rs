// Copyright 2023 The Fuchsia Authors
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

use core::arch::asm;
use static_assertions::const_assert_eq;

// Aarch64 supports aligned and unaligned stores to/from vector registers. Aligned accesses may be faster.
#[repr(align(16))]
#[derive(Clone, Copy, Default)]
struct AlignedU128(u128);

#[derive(Default)]
pub struct State {
    // [arm/v8]: A1.3.1 Execution state
    // 32 registers, 128 bits each
    q: [AlignedU128; 32],
    // [arm/v8]: A1.5 Advanced SIMD and floating-point support
    fpcr: u64,
    fpsr: u64,
}

const_assert_eq!(std::mem::size_of::<State>(), 512 + 16);

impl State {
    #[inline(always)]
    pub(crate) fn save(&mut self) {
        unsafe {
            asm!(
              "stp  q0,  q1, [{q}, #( 0 * 32)]",
              "stp  q2,  q3, [{q}, #( 1 * 32)]",
              "stp  q4,  q5, [{q}, #( 2 * 32)]",
              "stp  q6,  q7, [{q}, #( 3 * 32)]",
              "stp  q8,  q9, [{q}, #( 4 * 32)]",
              "stp q10, q11, [{q}, #( 5 * 32)]",
              "stp q12, q13, [{q}, #( 6 * 32)]",
              "stp q14, q15, [{q}, #( 7 * 32)]",
              "stp q16, q17, [{q}, #( 8 * 32)]",
              "stp q18, q19, [{q}, #( 9 * 32)]",
              "stp q20, q21, [{q}, #(10 * 32)]",
              "stp q22, q23, [{q}, #(11 * 32)]",
              "stp q24, q25, [{q}, #(12 * 32)]",
              "stp q26, q27, [{q}, #(13 * 32)]",
              "stp q28, q29, [{q}, #(14 * 32)]",
              "stp q30, q31, [{q}, #(15 * 32)]",
              q = in(reg) &self.q,
            );
            asm!(
              "mrs {fpcr}, fpcr",
              "mrs {fpsr}, fpsr",
              fpcr = out(reg) self.fpcr,
              fpsr = out(reg) self.fpsr,
            );
        }
    }

    #[inline(always)]
    // Safety: See comment in lib.rs.
    pub(crate) unsafe fn restore(&self) {
        asm!(
          "ldp  q0,  q1, [{q}, #( 0 * 32)]",
          "ldp  q2,  q3, [{q}, #( 1 * 32)]",
          "ldp  q4,  q5, [{q}, #( 2 * 32)]",
          "ldp  q6,  q7, [{q}, #( 3 * 32)]",
          "ldp  q8,  q9, [{q}, #( 4 * 32)]",
          "ldp q10, q11, [{q}, #( 5 * 32)]",
          "ldp q12, q13, [{q}, #( 6 * 32)]",
          "ldp q14, q15, [{q}, #( 7 * 32)]",
          "ldp q16, q17, [{q}, #( 8 * 32)]",
          "ldp q18, q19, [{q}, #( 9 * 32)]",
          "ldp q20, q21, [{q}, #(10 * 32)]",
          "ldp q22, q23, [{q}, #(11 * 32)]",
          "ldp q24, q25, [{q}, #(12 * 32)]",
          "ldp q26, q27, [{q}, #(13 * 32)]",
          "ldp q28, q29, [{q}, #(14 * 32)]",
          "ldp q30, q31, [{q}, #(15 * 32)]",
          "msr fpcr, {fpcr}",
          "msr fpsr, {fpsr}",
          q = in(reg) &self.q,
          fpcr = in(reg) self.fpcr,
          fpsr = in(reg) self.fpsr,
          out( "q0") _,
          out( "q1") _,
          out( "q2") _,
          out( "q3") _,
          out( "q4") _,
          out( "q5") _,
          out( "q6") _,
          out( "q7") _,
          out( "q8") _,
          out( "q9") _,
          out("q10") _,
          out("q11") _,
          out("q12") _,
          out("q13") _,
          out("q14") _,
          out("q15") _,
          out("q16") _,
          out("q17") _,
          out("q18") _,
          out("q19") _,
          out("q20") _,
          out("q21") _,
          out("q22") _,
          out("q23") _,
          out("q24") _,
          out("q25") _,
          out("q26") _,
          out("q27") _,
          out("q28") _,
          out("q29") _,
          out("q30") _,
          out("q31") _,
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

        // Declare and initialize the locals the tests below will use above as
        // the initialization for some of these uses vector registers that we
        // are manipulating as part of the tests.
        let mut custom_fpcr: u64;
        let dest = [0u8; 16];
        // v0-v7 (aka q0-q7) are used for parameter values and return values in the aarch64 calling convention.
        let q0 = [0x42u8; 16];
        let q1 = [0x43u8; 16];
        let q2 = [0x44u8; 16];
        // The bottom 64 bits of v8-v15 (aka q8-v15) are callee-preserved in the aarch64 calling convention.
        let q8 = [0x45u8; 16];
        // v16-v31 do not need to be preserved.
        let q20 = [0x46u8; 16];

        // Set custom state by hand
        {
            // [arm/v8]: C5.2.8 FPSR, Floating-point Status Register
            // Divide by zero to raise DZC (bit 1) in FPSR
            let one = 1.0;
            let zero = 0.0;
            unsafe {
                asm!(
                    "fdiv {dest:d}, {one:d}, {zero:d}",
                    one = in(vreg) one,
                    zero = in(vreg) zero,
                    dest = out(vreg) _
                );
            }
            let mut fpsr: u64;
            unsafe {
                asm!(
                    "mrs {fpsr}, fpsr",
                    fpsr = out(reg) fpsr,
                );
            }
            assert_eq!(fpsr, 1 << 1);

            // [arm/v8]: C5.2.7 FPCR, Floating-point Control Register
            // Control bits:
            //   AHP 26
            //   DN 25
            //   FZ 24
            //   RMode 23:22
            //   Stride 21:20
            //   FZ16 19
            //   Len 18:16 - no meaning in aarch64, used for context save/restore in aarch32 mode
            const FPCR_CONTROL_BITS: u64 = 0xff << 19;

            // Interrupt enable bits:
            //   IDE 15
            //   IXE 12
            //   UFE 11
            //   OFE 10
            //   DZE 9
            //   IOE 8
            const FPCR_INTERRUPT_ENABLE_BITS: u64 = 1 << 15 | 0x1f << 8;
            custom_fpcr = FPCR_CONTROL_BITS | FPCR_INTERRUPT_ENABLE_BITS;

            unsafe {
                asm!("msr fpcr, {fpcr}", fpcr = in(reg) custom_fpcr);
            }
            // The implementation may not support setting some of these bits. Read back fpcr to see
            // what the hardware actually allows so we can verify that it was restored later on.
            unsafe {
                asm!(
                    "mrs {fpcr}, fpcr",
                    fpcr = out(reg) custom_fpcr,
                );
            }

            // Load in known values to the first 3 vector registers, first callee preserved register, and non preserved register.
            unsafe {
                asm!("
                    ldr  q0,  [{q0}]
                    ldr  q1,  [{q1}]
                    ldr  q2,  [{q2}]
                    ldr  q8,  [{q8}]
                    ldr q20, [{q20}]
                    ",
                 q0 = in(reg) &q0,
                 q1 = in(reg) &q1,
                 q2 = in(reg) &q2,
                 q8 = in(reg) &q8,
                 q20 = in(reg) &q20,
                );
            }
        }

        state.save();

        // Clear state manually
        {
            let fpcr = 0u64;
            let fpsr = 0u64;
            unsafe {
                asm!("
                    msr fpcr, {fpcr}
                    msr fpsr, {fpsr}
                    ",
                    fpcr = in(reg) fpcr,
                    fpsr = in(reg) fpsr,
                );
                asm!(
                    "
                    eor  v0.16b,  v0.16b,  v0.16b
                    eor  v1.16b,  v1.16b,  v1.16b
                    eor  v2.16b,  v2.16b,  v2.16b
                    eor  v8.16b,  v8.16b,  v8.16b
                    eor v20.16b, v20.16b, v20.16b
                    "
                );
            }
        };

        // Verify that the state is cleared
        {
            let mut fpcr: u64;
            let mut fpsr: u64;
            unsafe {
                asm!(
                    "
                    mrs {fpcr}, fpcr
                    mrs {fpsr}, fpsr
                    ",
                    fpcr = out(reg) fpcr,
                    fpsr = out(reg) fpsr,
                );
            }
            assert_eq!(fpcr, 0);
            assert_eq!(fpsr, 0);

            unsafe {
                asm!("str q0, [{dest}]",
                     dest = in(reg) &dest);
            }
            for i in 0..16 {
                assert_eq!(dest[i], 0);
            }
            unsafe {
                asm!("str q1, [{dest}]",
                     dest = in(reg) &dest);
            }
            for i in 0..16 {
                assert_eq!(dest[i], 0);
            }
            unsafe {
                asm!("str q2, [{dest}]",
                     dest = in(reg) &dest);
            }
            for i in 0..16 {
                assert_eq!(dest[i], 0);
            }
            unsafe {
                asm!("str q8, [{dest}]",
                     dest = in(reg) &dest);
            }
            for i in 0..16 {
                assert_eq!(dest[i], 0);
            }
            unsafe {
                asm!("str q20, [{dest}]",
                     dest = in(reg) &dest);
            }
            for i in 0..16 {
                assert_eq!(dest[i], 0);
            }
        }

        unsafe {
            state.restore();
        }
        // Verify that the state restored to what we expect
        {
            let mut fpcr: u64;
            let mut fpsr: u64;
            unsafe {
                asm!("
                    mrs {fpcr}, fpcr
                    mrs {fpsr}, fpsr
                    ",
                    fpcr = out(reg) fpcr,
                    fpsr = out(reg) fpsr,
                );
            }
            assert_eq!(fpcr, custom_fpcr);
            assert_eq!(fpsr, 1 << 1);

            unsafe {
                asm!("str q0, [{dest}]",
                     dest = in(reg) &dest);
            }
            for i in 0..16 {
                assert_eq!(dest[i], 0x42);
            }
            unsafe {
                asm!("str q1, [{dest}]",
                     dest = in(reg) &dest);
            }
            for i in 0..16 {
                assert_eq!(dest[i], 0x43);
            }
            unsafe {
                asm!("str q2, [{dest}]",
                     dest = in(reg) &dest);
            }
            for i in 0..16 {
                assert_eq!(dest[i], 0x44);
            }
            unsafe {
                asm!("str q8, [{dest}]",
                     dest = in(reg) &dest);
            }
            for i in 0..16 {
                assert_eq!(dest[i], 0x45);
            }
            unsafe {
                asm!("str q20, [{dest}]",
                     dest = in(reg) &dest);
            }
            for i in 0..16 {
                assert_eq!(dest[i], 0x46);
            }
        }
    }
}
