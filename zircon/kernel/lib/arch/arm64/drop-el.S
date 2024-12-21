// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/arm64/system-asm.h>
#include <lib/arch/asm.h>

// The CPTR bits unknown to hwreg are those intended to be reserved as one.
#define CPTR_EL2_RES1 CPTR_EL2_UNKNOWN

// 0b101 signifies returning to EL1 with SP_EL1.
#define SPSR_EL2_M_EL1H SPSR_EL2_M_FIELD(0b101)
#define SPSR_EL3_M_EL1H SPSR_EL3_M_FIELD(0b101)

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

    // Set EL2 to 64bit and enable the HVC and SMC instructions.
    mrs x9, scr_el3
    mov x10, SCR_EL3_NS | SCR_EL3_HCE | SCR_EL3_RW
    orr x9, x9, x10
    bic x9, x9, SCR_EL3_SMD  // Double negative: clear Secure Monitor Disable
    msr scr_el3, x9

    // Set the return address and exception level.
    adr x9, .Ltarget
    msr elr_el3, x9
    mov x9, SPSR_EL3_D | SPSR_EL3_A |SPSR_EL3_I |SPSR_EL3_F | SPSR_EL3_M_EL1H
    msr spsr_el3, x9

    // Check to see if there's an EL2. If not, continue straight to the EL1
    // drop; if so, then we can configure it from EL3.
    mrs x9, id_aa64pfr0_el1
    and x9, x9, ID_AA64PFR0_EL1_EL2
    cbz x9, .Ldrop_to_el1

.Lconfigure_el2:
    // Clear any pre-existing EL2 exception handlers.
    msr vbar_el2, xzr

    // Set EL1 to 64bit, ensuring also that E2H is clear, as the assumed
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

    // Make sure MPIDR_EL1 and MIDR_EL1 are set correctly.
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
    msr ICC_SRE_EL2, x9

    // Disable the GIC virtual CPU interface.
    msr ich_hcr_el2, xzr

.Lno_gic_sysregs:
    // Set the return address and exception level.
    adr x9, .Ltarget
    msr elr_el2, x9
    mov x9, SPSR_EL2_D | SPSR_EL2_A |SPSR_EL2_I |SPSR_EL2_F | SPSR_EL2_M_EL1H
    msr spsr_el2, x9

.Ldrop_to_el1:
    // Install the current stack for reuse in EL1, if any.
    mov x9, sp
    msr sp_el1, x9

    isb
    eret
    speculation_postfence

.Ltarget:
   ret
   speculation_postfence
.end_function
