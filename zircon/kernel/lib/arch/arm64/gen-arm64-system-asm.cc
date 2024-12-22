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
      .Register<arch::ArmCnthctlEl2</*El2Host=*/false>>("CNTHCTL_EL2_")
      .Register<arch::ArmCptrEl2</*El2Host=*/false>>("CPTR_EL2_")
      .Register<arch::ArmCurrentEl>("CURRENT_EL_")
      .Register<arch::ArmHcrEl2>("HCR_EL2_")
      .Register<arch::ArmIccSreEl2>("ICC_SRE_EL2_")
      .Register<arch::ArmIdAa64Pfr0El1>("ID_AA64PFR0_EL1_")
      .Register<arch::ArmScrEl3>("SCR_EL3_")
      .Register<arch::ArmSctlrEl1>("SCTLR_EL1_")
      .Register<arch::ArmSctlrEl2>("SCTLR_EL2_")
      .Register<arch::ArmSpsrEl2>("SPSR_EL2_")
      .Register<arch::ArmSpsrEl3>("SPSR_EL3_")
      .Register<arch::ArmVbarEl2>("VBAR_EL2_")
      .Main(argc, argv);
}
