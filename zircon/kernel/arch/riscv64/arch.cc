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
#include <zircon/types.h>

#include <arch/mp.h>
#include <arch/ops.h>
#include <arch/regs.h>
#include <arch/riscv64/mmu.h>
#include <arch/riscv64/sbi.h>
#include <arch/vm.h>
#include <kernel/percpu.h>
#include <kernel/scheduler.h>
#include <kernel/thread.h>
#include <lk/init.h>
#include <lk/main.h>

// include this at least once in C++ code to make sure the static assert is valid
#include <phys/arch/arch-handoff-asm.h>

#define LOCAL_TRACE 0

// first C level code to initialize each cpu
void riscv64_init_percpu() {
  // set the top level exception handler
  riscv64_csr_write(RISCV64_CSR_STVEC, (uintptr_t)&riscv64_exception_entry);

  // set up the default sstatus for the current cpu
  riscv64_csr_write(RISCV64_CSR_SSTATUS, 0);

  // enable software interrupts and external interrupts, disable everything else
  riscv64_csr_write(RISCV64_CSR_SIE, RISCV64_CSR_SIE_SIE | RISCV64_CSR_SIE_EIE);

  // enable all of the counters
  riscv64_csr_write(RISCV64_CSR_SCOUNTEREN, RISCV64_CSR_SCOUNTEREN_CY | RISCV64_CSR_SCOUNTEREN_TM |
                                                RISCV64_CSR_SCOUNTEREN_IR);

  // Zero out the fpu state and set to initial
  riscv64_fpu_zero();
}

// Called in start.S prior to entering the main kernel.
// Bootstraps the boot cpu as cpu 0 intrinsically, though it may have a nonzero hart.
extern "C" void riscv64_boot_cpu_init(uint32_t hart_id) {
  riscv64_init_percpu();
  riscv64_mp_early_init_percpu(hart_id, 0);
}

void arch_early_init() {
  riscv64_sbi_early_init();
  riscv64_mmu_early_init();
  riscv64_mmu_early_init_percpu();

  // mark the boot cpu online
  mp_set_curr_cpu_online(true);
}

void arch_prevm_init() {}

void arch_init() TA_NO_THREAD_SAFETY_ANALYSIS {
  // print some arch info
  dprintf(INFO, "RISCV: Boot HART ID %u\n", riscv64_boot_hart_id());
  dprintf(INFO, "RISCV: Supervisor mode\n");

  riscv64_sbi_init();

  riscv64_mmu_init();
}

void arch_late_init_percpu() {
  // per cpu on each secondary (and the boot cpu a second time)
  mp_set_curr_cpu_online(true);
}

__NO_RETURN int arch_idle_thread_routine(void*) {
  for (;;) {
    __asm__ volatile("wfi");
  }
}
