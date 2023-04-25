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

// TODO-rvbringup: move secondary cpu stuff to mp.cc

// Used to hold up the boot sequence on secondary CPUs until signaled by the primary.
static ktl::atomic<bool> secondaries_released;

// one for each secondary CPU, indexed by (cpu_num - 1).
static Thread _init_thread[SMP_MAX_CPUS - 1];

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
void riscv64_cpu_early_init() {
  // set the top level exception handler
  riscv64_csr_write(RISCV64_CSR_STVEC, (uintptr_t)&riscv64_exception_entry);

  // mask all exceptions, just in case
  riscv64_csr_clear(RISCV64_CSR_SSTATUS, RISCV64_CSR_SSTATUS_IE);
  riscv64_csr_clear(RISCV64_CSR_SIE,
                    RISCV64_CSR_SIE_SIE | RISCV64_CSR_SIE_TIE | RISCV64_CSR_SIE_EIE);

  // Zero out the fpu state and set to initial
  riscv64_fpu_zero();
}

void arch_early_init() {
  riscv64_cpu_early_init();
  riscv64_mmu_early_init();
}

void arch_prevm_init() {}

void arch_init() TA_NO_THREAD_SAFETY_ANALYSIS {
  // print some arch info
  dprintf(INFO, "RISCV: Supervisor mode\n");
  dprintf(INFO, "RISCV: mvendorid %#lx marchid %#lx mimpid %#lx\n",
          sbi_call(SBI_GET_MVENDORID).value, sbi_call(SBI_GET_MARCHID).value,
          sbi_call(SBI_GET_MIMPID).value);
  dprintf(INFO, "RISCV: SBI impl id %#lx version %#lx\n", sbi_call(SBI_GET_SBI_IMPL_ID).value,
          sbi_call(SBI_GET_SBI_IMPL_VERSION).value);

  // probe some SBI extensions
  dprintf(INFO, "RISCV: SBI extension TIMER %ld\n",
          sbi_call(SBI_PROBE_EXTENSION, SBI_EXT_TIMER).value);
  dprintf(INFO, "RISCV: SBI extension IPI %ld\n", sbi_call(SBI_PROBE_EXTENSION, SBI_EXT_IPI).value);
  dprintf(INFO, "RISCV: SBI extension RFENCE %ld\n",
          sbi_call(SBI_PROBE_EXTENSION, SBI_EXT_RFENCE).value);
  dprintf(INFO, "RISCV: SBI extension HSM %ld\n", sbi_call(SBI_PROBE_EXTENSION, SBI_EXT_HSM).value);

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

void arch_late_init_percpu(void) {
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

void arch_setup_uspace_iframe(iframe_t* iframe, uintptr_t pc, uintptr_t sp, uintptr_t arg1,
                              uintptr_t arg2) {
  iframe->regs.pc = pc;
  iframe->regs.sp = sp;
  iframe->regs.a0 = arg1;
  iframe->regs.a1 = arg2;

  // Saved interrupt enable (so that interrupts are enabled when returning to user space).
  // Current interrupt enable state is also disabled, which will matter when the
  // arch_uspace_entry loads sstatus temporarily before switching to user space.
  // Set user space bitness to 64bit.
  // Set the fpu to the initial state, with the implicit assumption that the context switch
  // routine would have defaulted the FPU state a the time this thread enters user space.
  // All other bits set to zero, default options.
  iframe->status =
      RISCV64_CSR_SSTATUS_PIE | RISCV64_CSR_SSTATUS_UXL_64BIT | RISCV64_CSR_SSTATUS_FS_INITIAL;
}

// Switch to user mode, set the user stack pointer to user_stack_top, save the
// top of the kernel stack pointer.
void arch_enter_uspace(iframe_t* iframe) {
  DEBUG_ASSERT(arch_ints_disabled());

  Thread* ct = Thread::Current::Get();

  LTRACEF("riscv64_uspace_entry(%#" PRIxPTR ", %#" PRIxPTR ", %#" PRIxPTR ", %#" PRIxPTR ")\n",
          iframe->regs.a0, iframe->regs.a1, ct->stack().top(), iframe->regs.pc);

  ASSERT(arch_is_valid_user_pc(iframe->regs.pc));

#if __has_feature(shadow_call_stack)
  riscv64_uspace_entry(iframe, ct->stack().top(), ct->stack().shadow_call_base());
#else
  riscv64_uspace_entry(iframe, ct->stack().top());
#endif
  __UNREACHABLE;
}

// TODO-rvbringup: move to mp.cc
extern "C" void riscv64_secondary_entry(uint hart_id, uint cpu_num) {
  riscv64_init_percpu_early(hart_id, cpu_num);
  riscv64_cpu_early_init();

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
