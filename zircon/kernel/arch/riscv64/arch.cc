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

namespace {

// Used to hold up the boot sequence on secondary CPUs until signaled by the primary.
ktl::atomic<bool> secondaries_released;

// one for each secondary CPU, indexed by (cpu_num - 1).
Thread _init_thread[SMP_MAX_CPUS - 1];

uint32_t riscv64_boot_hart;

}  // anonymous namespace

zx_status_t riscv64_create_secondary_stack(cpu_num_t cpu_num, vaddr_t* sp) {
  DEBUG_ASSERT_MSG(cpu_num > 0 && cpu_num < SMP_MAX_CPUS, "cpu_num: %u", cpu_num);
  KernelStack* stack = &_init_thread[cpu_num - 1].stack();
  DEBUG_ASSERT(stack->base() == 0);
  zx_status_t status = stack->Init();
  if (status != ZX_OK) {
    return status;
  }

  // Store cpu_num on the stack
  *(((uint64_t*)stack->top()) - 1) = cpu_num;

  // Store the stack pointer for our caller
  *sp = stack->top();

  return ZX_OK;
}

zx_status_t riscv64_free_secondary_stack(cpu_num_t cpu_num) {
  DEBUG_ASSERT(cpu_num > 0 && cpu_num < SMP_MAX_CPUS);
  return _init_thread[cpu_num - 1].stack().Teardown();
}

// first C level code to initialize each cpu
void riscv64_init_percpu() {
  // set the top level exception handler
  riscv64_csr_write(RISCV64_CSR_STVEC, (uintptr_t)&riscv64_exception_entry);

  // set up the default sstatus for the current cpu
  riscv64_csr_write(RISCV64_CSR_SSTATUS, 0);

  // mask all exceptions, just in case
  riscv64_csr_clear(RISCV64_CSR_SIE,
                    RISCV64_CSR_SIE_SIE | RISCV64_CSR_SIE_TIE | RISCV64_CSR_SIE_EIE);

  // enable all of the counters
  riscv64_csr_write(RISCV64_CSR_SCOUNTEREN, RISCV64_CSR_SCOUNTEREN_CY | RISCV64_CSR_SCOUNTEREN_TM |
                                                RISCV64_CSR_SCOUNTEREN_IR);

  // Zero out the fpu state and set to initial
  riscv64_fpu_zero();
}

// Called in start.S prior to entering the main kernel.
// Bootstraps the boot cpu as cpu 0 intrinsically, though it may have a nonzero hart.
extern "C" void riscv64_boot_cpu_init(uint32_t hart_id) {
  riscv64_boot_hart = hart_id;
  riscv64_init_percpu();
  riscv64_mp_early_init_percpu(hart_id, 0);
}

void arch_early_init() {
  riscv64_sbi_early_init();
  riscv64_mmu_early_init();
  riscv64_mmu_early_init_percpu();
}

void arch_prevm_init() {}

void arch_init() TA_NO_THREAD_SAFETY_ANALYSIS {
  // print some arch info
  dprintf(INFO, "RISCV: Boot HART ID %u\n", riscv64_boot_hart);
  dprintf(INFO, "RISCV: Supervisor mode\n");

  riscv64_sbi_init();

  riscv64_mmu_init();

  // TODO-rvbringup: this should move to platform
#if 0
  uint32_t max_cpus = arch_max_num_cpus();
  int secondaries_to_init = max_cpus;

  lk_init_secondary_cpus(secondaries_to_init);

  LTRACEF("releasing %d secondary cpus\n", secondaries_to_init);
  secondaries_released.store(true);
#endif
}

void arch_late_init_percpu() {
  // enable software interrupts, used for inter-processor-interrupts
  riscv64_csr_set(RISCV64_CSR_SIE, RISCV64_CSR_SIE_SIE);

  // enable external interrupts
  riscv64_csr_set(RISCV64_CSR_SIE, RISCV64_CSR_SIE_EIE);

  mp_set_curr_cpu_online(true);
}

__NO_RETURN int arch_idle_thread_routine(void*) {
  for (;;) {
    __asm__ volatile("wfi");
  }
}

extern "C" void riscv64_secondary_entry(uint32_t hart_id, uint cpu_num) {
  riscv64_init_percpu();
  riscv64_mp_early_init_percpu(hart_id, cpu_num);
  riscv64_mmu_early_init_percpu();

  // Wait until the primary has finished setting things up.
  while (!secondaries_released.load()) {
    arch::Yield();
  }

  _init_thread[cpu_num - 1].SecondaryCpuInitEarly();
  // Run early secondary cpu init routines up to the threading level.
  lk_init_level(LK_INIT_FLAG_SECONDARY_CPUS, LK_INIT_LEVEL_EARLIEST, LK_INIT_LEVEL_THREADING - 1);

  arch_mp_init_percpu();

  dprintf(INFO, "RISCV: secondary cpu %u coming up\n", cpu_num);

  lk_secondary_cpu_entry();
}
