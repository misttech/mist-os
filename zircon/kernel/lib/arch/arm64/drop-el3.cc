// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/arch/arm64/feature.h>
#include <lib/arch/arm64/system.h>

namespace arch {

ArmCurrentEl ArmDropEl3() {
  auto current_el = ArmCurrentEl::Read();
  if (current_el.el() < 3) {
    return current_el;
  }

  bool has_el2 = ArmIdAa64Pfr0El1::Read().el2() != ArmIdAa64Pfr0El1::El::kNone;

  // Set up the basics in SCR_EL3 for enabling the next lower EL.
  ArmScrEl3::Modify([has_el2](auto& scr) {
    scr.set_rw(true)        // Next lower EL is AArch64.
        .set_ns(true)       // EL<3 in Non-Secure state, no use Secure memory.
        .set_hce(has_el2);  // Enable HVC insn to escape from EL1 to EL2.
  });

  // Set up SPSR_EL3 so the ERET instruction will return to the next lower EL.
  ArmSpsrEl3::Modify([has_el2](auto& spsr) {
    // Return to ELx, using SP_ELx as SP.
    spsr.set_m(has_el2 ? ArmSavedProgramStatusRegister::ExceptionLevel::kEl2h
                       : ArmSavedProgramStatusRegister::ExceptionLevel::kEl1h)
        // Disable all kinds of exceptions.
        .set_d(true)
        .set_a(true)
        .set_i(true)
        .set_f(true);
  });

  // Do an ERET "in place", so it restores the SP and PC that we're already
  // using.  The only difference between dropping to EL2 and to EL1 here is
  // whether SP_EL2 or SP_EL1 will be used.  When EL2 is not supported, it's
  // harmless to set SP_EL2 anyway.  When EL2 is supported, we clobber SP_EL1
  // on the presumption that EL2 code will be resetting EL1 state anyway.
  uint64_t sp, pc;
  __asm__ volatile(
      R"""(
        adr %[pc], 0f
        mov %[sp], sp
        msr elr_el3, %[pc]
        msr sp_el2, %[sp]
        msr sp_el1, %[sp]
        isb
        eret
      0:
      )"""
      : [sp] "=r"(sp), [pc] "=r"(pc));

  // Return the new EL we're running at now.
  current_el = ArmCurrentEl::Read();
  ZX_DEBUG_ASSERT(current_el.el() == (has_el2 ? 2 : 1));
  return current_el;
}

}  // namespace arch
