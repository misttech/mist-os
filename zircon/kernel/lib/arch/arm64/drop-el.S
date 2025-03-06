// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/arm64/feature-asm.h>
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
  // Clear the vector table for EL2.
  msr vbar_el2, xzr

  // Check for FEAT_HCX.
  mrs x9, id_aa64mmfr1_el1
  tst x9, #ID_AA64MMFR1_EL1_HCX
  beq .Lno_hcx

  // Set HCRX_EL2 bits so nothing traps to EL2 and EL2 doesn't constrain what
  // EL1 can do.  Unfortunately, the 0 state is not always that state.  Several
  // bits should be 1, but only if some FEAT_* is available.  The comments in
  // <lib/arch/arm64/system.h> for each HCRX_EL2 bit say whether each bit
  // should be set for some FEAT_*.  If more HCRX_EL2 bits are defined, then
  // new `set_hcrx_bits` invocations should be added here.

  .macro set_hcrx_bits feature_reg, feature_mask, set_bit
    mov scratch, \set_bit
    tst \feature_reg, \feature_mask
    csel scratch, scratch, xzr, ne
    orr hcrx_bits, hcrx_bits, scratch
  .endm

  hcrx_bits .req x9
  isar1 .req x10
  isar2 .req x11
  mmfr3 .req x12
  pfr1 .req x13
  scratch .req x14

  mov hcrx_bits, xzr
  mrs isar1, id_aa64isar2_el1
  mrs isar2, id_aa64isar2_el1
  mrs mmfr3, id_aa64mmfr3_el1
  mrs pfr1, id_aa64pfr1_el1

  // FEAT_SYSREG128
  set_hcrx_bits isar2, #ID_AA64ISAR2_EL1_SYSREG_128, #HCRX_EL2_ENIDCP128

  // FEAT_D128
  set_hcrx_bits mmfr3, #ID_AA64MMFR3_EL1_D128, #HCRX_EL2_D128EN

  // FEAT_THE
  set_hcrx_bits pfr1, #ID_AA64PFR1_EL1_THE, #HCRX_EL2_PTTWI

  // FEAT_SCTLR2
  set_hcrx_bits mmfr3, #ID_AA64MMFR3_EL1_SCTLRX, #HCRX_EL2_SCTLR2EN

  // FEAT_TCR2
  set_hcrx_bits mmfr3, #ID_AA64MMFR3_EL1_TCRX, #HCRX_EL2_TCR2EN

  // FEAT_MOPS
  set_hcrx_bits isar2, #ID_AA64ISAR2_EL1_MOPS, #HCRX_EL2_MSCEN

  // FEAT_LS64
  set_hcrx_bits isar1, #ID_AA64ISAR1_EL1_LS64, \
           #(HCRX_EL2_ENASR | HCRX_EL2_ENALS | HCRX_EL2_ENAS0)

  .arch armv8.7-a
  msr hcrx_el2, hcrx_bits
.Lno_hcx:

  // Set EL1 to 64-bit, ensuring also that E2H is clear, as the assumed
  // layouts of CNTHCTL_EL2 and CPTR_EL2 depend on that. Also, ensure
  // that TSC is clear so that `smc` traps in EL3.
  mov x9, #HCR_EL2_RW
  msr hcr_el2, x9

  mrs x9, cnthctl_el2
  // Disable EL1 timer traps and the timer offset.
  orr x9, x9, #CNTHCTL_EL2_EL1PCEN | CNTHCTL_EL2_EL1PCTEN
  // Make sure the EL2 physical event stream is not running.
  bic x9, x9, #CNTHCTL_EL2_EVNTEN
  msr cnthctl_el2, x9
  msr cntvoff_el2, xzr

  // Disable stage 2 translations.
  msr vttbr_el2, xzr

  // Disable EL2 coprocessor traps.
  mov x9, #CPTR_EL2_RES1
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
  and x9, x9, #ID_AA64PFR0_EL1_GIC
  cbz x9, .Lno_gic_sysregs

  // Enable the GIC system registers in EL2, and allow their use in EL1.
  mrs x9, icc_sre_el2
  mov x10, #ICC_SRE_EL2_ENABLE | ICC_SRE_EL2_SRE
  orr x9, x9, x10
  msr icc_sre_el2, x9

  // Disable the GIC virtual CPU interface.
  msr ich_hcr_el2, xzr

.Lno_gic_sysregs:
  // Set the return address and exception level.
  adr x9, .Ltarget
  msr elr_el2, x9
  mov x9, #SPSR_EL2_DAIF | SPSR_EL2_M_EL1H
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
