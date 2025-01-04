// Copyright 2021 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/arm64/feature.h>
#include <lib/arch/arm64/system.h>

#include <hwreg/asm.h>

int main(int argc, char** argv) {
  return hwreg::AsmHeader()  //
      .Register<arch::ArmCnthctlEl2NoEl2Host>("CNTHCTL_EL2_")
      .Register<arch::ArmCptrEl2NoEl2Host>("CPTR_EL2_")
      // The CPTR bits unknown to hwreg are those intended to be reserved as one.
      .Macro("CPTR_EL2_RES1", "CPTR_EL2_UNKNOWN")
      .Register<arch::ArmCurrentEl>("CURRENT_EL_")
      .Register<arch::ArmHcrEl2>("HCR_EL2_")
      .Register<arch::ArmIccSreEl2>("ICC_SRE_EL2_")
      .Register<arch::ArmIdAa64Pfr0El1>("ID_AA64PFR0_EL1_")
      .Register<arch::ArmSctlrEl1>("SCTLR_EL1_")
      .Register<arch::ArmSctlrEl2>("SCTLR_EL2_")
      .Register<arch::ArmSpsrEl2>("SPSR_EL2_")
      // Disables all interrupts.
      .Macro("SPSR_EL2_DAIF", "SPSR_EL2_D | SPSR_EL2_A | SPSR_EL2_I | SPSR_EL2_F")
      // 0b101 signifies returning to EL1 with SP_EL1.
      .Macro("SPSR_EL2_M_EL1H", "SPSR_EL2_M_FIELD(0b101)")
      .Main(argc, argv);
}
