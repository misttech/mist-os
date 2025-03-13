// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include <arch.h>
#include <assert.h>
#include <bits.h>
#include <debug.h>
#include <inttypes.h>
#include <lib/arch/intrin.h>
#include <platform.h>
#include <stdlib.h>
#include <string.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/time.h>
#include <zircon/types.h>

#include <arch/mp.h>
#include <arch/ops.h>
#include <arch/regs.h>
#include <arch/riscv64/feature.h>
#include <arch/riscv64/mmu.h>
#include <arch/riscv64/sbi.h>
#include <arch/vm.h>
#include <kernel/percpu.h>
#include <kernel/scheduler.h>
#include <kernel/thread.h>
#include <lk/init.h>
#include <lk/main.h>
#include <phys/handoff.h>

#define LOCAL_TRACE 0

// first C level code to initialize each cpu
void riscv64_init_percpu() {
  // set the top level exception handler
  riscv64_csr_write(RISCV64_CSR_STVEC, (uintptr_t)&riscv64_exception_entry);

  // set up the default sstatus for the current cpu
  riscv64_csr_write(RISCV64_CSR_SSTATUS, 0);

  // enable software interrupts and external interrupts, disable everything else
  riscv64_csr_write(RISCV64_CSR_SIE, RISCV64_CSR_SIE_SSIE | RISCV64_CSR_SIE_SEIE);

  // enable all of the counters
  riscv64_csr_write(RISCV64_CSR_SCOUNTEREN, RISCV64_CSR_SCOUNTEREN_CY | RISCV64_CSR_SCOUNTEREN_TM |
                                                RISCV64_CSR_SCOUNTEREN_IR);

  // Zero out the fpu state, and set to initial
  riscv64_fpu_zero();
}

// Called in start.S prior to entering the main kernel.
// Bootstraps the boot cpu as cpu 0 intrinsically, though it may have a nonzero hart.
extern "C" void riscv64_boot_cpu_init(const PhysHandoff* handoff) {
  uint32_t hart_id = static_cast<uint32_t>(handoff->arch_handoff.boot_hart_id);
  riscv64_init_percpu();
  riscv64_mp_early_init_percpu(hart_id, 0);
}

void arch_early_init() {
  riscv64_sbi_early_init();
  riscv64_feature_early_init();
  riscv64_mmu_early_init();
  riscv64_mmu_early_init_percpu();

  // mark the boot cpu online
  mp_set_curr_cpu_online(true);
}

void arch_prevm_init() { riscv64_mmu_prevm_init(); }

void arch_init() TA_NO_THREAD_SAFETY_ANALYSIS {
  // print some arch info
  dprintf(INFO, "RISCV: Boot HART ID %u\n", riscv64_boot_hart_id());
  dprintf(INFO, "RISCV: Supervisor mode\n");

  riscv64_feature_init();

  riscv64_sbi_init();

  riscv64_mmu_init();
}

void arch_late_init_percpu() {
  // While it would be nicer to zero out vector state - and set it to initial -
  // earlier and next to the call to do so for FPU state, that is too early for
  // vector feature bit to have been set.
  if (gRiscvFeatures[arch::RiscvFeature::kVector]) {
    riscv64_vector_zero();
  }

  if (gRiscvFeatures[arch::RiscvFeature::kZicbom]) {
    // allow userspace to perform FLUSH and CLEAN operations, but forbid INVAL
    riscv64_csr_set(RISCV64_CSR_SENVCFG, RISCV64_CSR_SENVCFG_CBCFE);
    riscv64_csr_clear(RISCV64_CSR_SENVCFG, RISCV64_CSR_SENVCFG_CBIE_MASK);
    riscv64_csr_set(RISCV64_CSR_SENVCFG, RISCV64_CSR_SENVCFG_CBIE_ILLEGAL);
  }
  if (gRiscvFeatures[arch::RiscvFeature::kZicboz]) {
    // Allow user space to perform zeroing
    riscv64_csr_set(RISCV64_CSR_SENVCFG, RISCV64_CSR_SENVCFG_CBZE);
  }

  // per cpu on each secondary (and the boot cpu a second time)
  mp_set_curr_cpu_online(true);
}

void ArchIdlePowerThread::EnterIdleState(zx_duration_t max_latency) { __wfi(); }
