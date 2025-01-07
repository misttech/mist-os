// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/arm64/system-asm.h>
#include <lib/arch/asm.h>

.text

// void ArmDropToEl1WithoutEl2Monitor()
.function ArmDropToEl1WithoutEl2Monitor, global
  mrs x9, CurrentEL

  // Check the current exception level.
  cmp x9, CURRENT_EL_EL_FIELD(1)
  beq .Ltarget
  cmp x9, CURRENT_EL_EL_FIELD(2)
  beq .Lconfigure_el2
  // Otherwise, we are in EL3.
  //
  // Throw a fatal exception for now; support will be added for this case when
  // it can be developed against hardware that drops us there.
  udf #0

.Lconfigure_el2:
  // Clear the init vector table for EL2.
  msr vbar_el2, xzr

  // Set EL1 to 64-bit, ensuring also that E2H is clear, as the assumed
  // layouts of CNTHCTL_EL2 and CPTR_EL2 depend on that. Also, ensure
  // that TSC is clear so that `smc` traps in EL3.
  mov x9, HCR_EL2_RW
  msr hcr_el2, x9

  mrs x9, cnthctl_el2
  // Disable EL1 timer traps and the timer offset.
  orr x9, x9, CNTHCTL_EL2_EL1PCEN | CNTHCTL_EL2_EL1PCTEN
  // Make sure the EL2 physical event stream is not running.
  bic x9, x9, CNTHCTL_EL2_EVNTEN
  msr cnthctl_el2, x9
  msr cntvoff_el2, xzr

  // Disable stage 2 translations.
  msr vttbr_el2, xzr

  // Disable EL2 coprocessor traps.
  mov x9, CPTR_EL2_RES1
  msr cptr_el2, x9

  // VMPIDR_EL2 and VPIDR_EL2 are what are returned from reads of MPIDR_EL1 and
  // MIDR_EL2 when in non-secure EL1, respectively. Reads of MPIDR_EL1 and
  // MIDR_EL1 when in EL2 return the real values, so we make sure now that
  // their VM*_EL2 counterparts reflect those values too.
  mrs x9, mpidr_el1
  msr vmpidr_el2, x9
  mrs x9, midr_el1
  msr vpidr_el2, x9

  // Check whether the GIC system registers are supported.
  mrs x9, id_aa64pfr0_el1
  and x9, x9, ID_AA64PFR0_EL1_GIC
  cbz x9, .Lno_gic_sysregs

  // Enable the GIC system registers in EL2, and allow their use in EL1.
  mrs x9, icc_sre_el2
  mov x10, ICC_SRE_EL2_ENABLE | ICC_SRE_EL2_SRE
  orr x9, x9, x10
  msr icc_sre_el2, x9

  // Disable the GIC virtual CPU interface.
  msr ich_hcr_el2, xzr

.Lno_gic_sysregs:
  // Set the return address and exception level.
  adr x9, .Ltarget
  msr elr_el2, x9
  mov x9, SPSR_EL2_DAIF | SPSR_EL2_M_EL1H
  msr spsr_el2, x9

.Ldrop_to_el1:
  // Reuse the current stack pointer after the drop to EL1.
  mov x9, sp
  msr sp_el1, x9

  isb
  eret
  speculation_postfence

.Ltarget:
  ret
  speculation_postfence
.end_function
