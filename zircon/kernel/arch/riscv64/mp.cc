// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include "arch/riscv64/mp.h"

#include <assert.h>
#include <lib/arch/intrin.h>
#include <platform.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <arch/mp.h>
#include <arch/ops.h>
#include <arch/riscv64.h>
#include <arch/riscv64/mmu.h>
#include <arch/riscv64/sbi.h>
#include <dev/interrupt.h>
#include <kernel/event.h>
#include <lk/init.h>
#include <lk/main.h>
#include <vm/vm.h>

#define LOCAL_TRACE 0

// total number of detected cpus, accessed via arch_max_num_cpus()
uint riscv64_num_cpus = 1;

namespace {

// mapping of cpu -> hart
// kept separate from the percpu array for speed purposes
uint32_t cpu_to_hart_map[SMP_MAX_CPUS] = {0};

// per cpu structures, each cpu will point to theirs using the fixed register
riscv64_percpu riscv64_percpu_array[SMP_MAX_CPUS];

// local helper routine to help convert cpu masks to hart masks
template <typename Callback>
void for_every_hart_in_cpu_mask(cpu_mask_t cmask, Callback callback) {
  for (cpu_num_t cpu = 0; cpu < SMP_MAX_CPUS && cmask; cpu++, cmask >>= 1) {
    if (cmask & 1) {
      auto hart = cpu_to_hart_map[cpu];
      callback(hart, cpu);
    }
  }
}

// one for each secondary CPU, indexed by (cpu_num - 1).
Thread _init_thread[SMP_MAX_CPUS - 1];

}  // anonymous namespace

// software triggered exceptions, used for cross-cpu calls
void riscv64_software_exception() {
  DEBUG_ASSERT(arch_curr_cpu_num() < SMP_MAX_CPUS);

  // Clear the IPI by clearing the pending software IPI bit.
  riscv64_csr_clear(RISCV64_CSR_SIP, RISCV64_CSR_SIP_SIP);

  rmb();
  uint32_t reason = riscv64_read_percpu_ptr()->ipi_data.exchange(0);
  LTRACEF("current_cpu %u (hart %u) reason %#x\n", arch_curr_cpu_num(), riscv64_curr_hart_id(),
          reason);

  if (reason & (1u << MP_IPI_RESCHEDULE)) {
    mp_mbx_reschedule_irq(nullptr);
    reason &= ~(1u << MP_IPI_RESCHEDULE);
  }
  if (reason & (1u << MP_IPI_GENERIC)) {
    mp_mbx_generic_irq(nullptr);
    reason &= ~(1u << MP_IPI_GENERIC);
  }
  if (reason & (1u << MP_IPI_INTERRUPT)) {
    mp_mbx_interrupt_irq(nullptr);
    reason &= ~(1u << MP_IPI_INTERRUPT);
  }
  if (reason & (1u << MP_IPI_HALT)) {
    // Park this core in a WFI loop
    arch_disable_ints();
    mb();
    while (true) {
      __asm__("wfi" ::: "memory");
    }

    reason &= ~(1u << MP_IPI_HALT);
  }

  if (unlikely(reason)) {
    panic("RISCV: unhandled ipi cause %#x, cpu %u (hart %u)\n", reason, arch_curr_cpu_num(),
          riscv64_curr_hart_id());
  }
}

void arch_prepare_current_cpu_idle_state(bool idle) {
  // no-op
}

void arch_mp_reschedule(cpu_mask_t mask) {
  arch_mp_send_ipi(MP_IPI_TARGET_MASK, mask, MP_IPI_RESCHEDULE);
}

arch::HartMask riscv64_cpu_mask_to_hart_mask(cpu_mask_t cmask) {
  arch::HartMask hmask = 0;

  for_every_hart_in_cpu_mask(cmask, [&hmask](arch::HartId hart, cpu_num_t) {
    // set the bit in the hart mask
    hmask |= (1UL << hart);
  });

  return hmask;
}

void arch_mp_send_ipi(const mp_ipi_target_t target, cpu_mask_t cpu_mask, const mp_ipi_t ipi) {
  LTRACEF("target %d mask %#x, ipi %d\n", target, cpu_mask, ipi);

  arch::HartMask hart_mask = 0;
  arch::HartMaskBase hart_mask_base = 0;

  // translate the high level target + mask mechanism into just a mask
  switch (target) {
    case MP_IPI_TARGET_ALL:
      cpu_mask = mp_get_online_mask();
      break;
    case MP_IPI_TARGET_ALL_BUT_LOCAL:
      cpu_mask = mp_get_online_mask() & ~cpu_num_to_mask(arch_curr_cpu_num());
      break;
    case MP_IPI_TARGET_MASK:
      break;
    default:
      DEBUG_ASSERT(0);
  }

  // translate the cpu mask to a list of harts and set the hart mask and set the
  // pending ipi bit in the per cpu struct
  for_every_hart_in_cpu_mask(cpu_mask, [&hart_mask, ipi](arch::HartId hart, cpu_num_t cpu) {
    // record a pending hart to notify
    hart_mask |= (1UL << hart);

    // mark the pending ipi in the cpu
    riscv64_percpu_array[cpu].ipi_data |= (1U << ipi);
  });

  mb();
  LTRACEF("sending to hart_mask %#lx\n", hart_mask);
  arch::RiscvSbiRet ret = sbi_send_ipi(hart_mask, hart_mask_base);
  DEBUG_ASSERT(ret.error == arch::RiscvSbiError::kSuccess);
}

// Called once per cpu, sets up the percpu structure and tracks cpu number to hart id.
void riscv64_mp_early_init_percpu(uint32_t hart_id, cpu_num_t cpu_num) {
  riscv64_percpu_array[cpu_num].cpu_num = cpu_num;
  riscv64_percpu_array[cpu_num].hart_id = hart_id;
  riscv64_set_percpu(&riscv64_percpu_array[cpu_num]);
  cpu_to_hart_map[cpu_num] = hart_id;
  wmb();
}

void arch_mp_init_percpu() { interrupt_init_percpu(); }

void arch_flush_state_and_halt(Event* flush_done) {
  DEBUG_ASSERT(arch_ints_disabled());
  Thread::Current::Get()->preemption_state().PreemptDisable();
  flush_done->Signal();
  platform_halt_cpu();
  panic("control should never reach here\n");
}

zx_status_t arch_mp_prep_cpu_unplug(uint cpu_id) {
  if (cpu_id == 0 || cpu_id >= riscv64_num_cpus) {
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t arch_mp_cpu_unplug(uint cpu_id) {
  // we do not allow unplugging the bootstrap processor
  if (cpu_id == 0 || cpu_id >= riscv64_num_cpus) {
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t arch_mp_cpu_hotplug(cpu_num_t cpu_id) { return ZX_ERR_NOT_SUPPORTED; }

void arch_setup_percpu(cpu_num_t cpu_num, struct percpu* percpu) {
  riscv64_percpu* arch_percpu = &riscv64_percpu_array[cpu_num];
  DEBUG_ASSERT(arch_percpu->high_level_percpu == nullptr);
  arch_percpu->high_level_percpu = percpu;
}

uint32_t riscv64_boot_hart_id() { return riscv64_percpu_array[0].hart_id; }

// Routines dealing with starting secondary cpus.

namespace {

zx_status_t riscv64_create_secondary_stack(cpu_num_t cpu_num, vaddr_t* sp) {
  DEBUG_ASSERT_MSG(cpu_num > 0 && cpu_num < SMP_MAX_CPUS, "cpu_num: %u", cpu_num);

  // Allocate stack(s) for the per cpu init thread.
  KernelStack* stack = &_init_thread[cpu_num - 1].stack();
  DEBUG_ASSERT(stack->base() == 0);

  zx_status_t status = stack->Init();
  if (status != ZX_OK) {
    return status;
  }

  // Store cpu_num on the main stack.
  uint64_t* stack_top = reinterpret_cast<uint64_t*>(stack->top());
  stack_top[-1] = cpu_num;

  // Store the shadow call stack base on the stack
#if __has_feature(shadow_call_stack)
  stack_top[-2] = stack->shadow_call_base();
#endif

  // Return the stack pointer to our caller.
  *sp = stack->top();

  return ZX_OK;
}

zx_status_t riscv64_free_secondary_stack(cpu_num_t cpu_num) {
  DEBUG_ASSERT(cpu_num > 0 && cpu_num < SMP_MAX_CPUS);
  return _init_thread[cpu_num - 1].stack().Teardown();
}

}  // anonymous namespace

// Called from secondary cpu assembly trampoline.
extern "C" void riscv64_secondary_entry(uint32_t hart_id, uint cpu_num) {
  riscv64_init_percpu();
  riscv64_mp_early_init_percpu(hart_id, cpu_num);
  riscv64_mmu_early_init_percpu();

  _init_thread[cpu_num - 1].SecondaryCpuInitEarly();
  // Run early secondary cpu init routines up to the threading level.
  lk_init_level(LK_INIT_FLAG_SECONDARY_CPUS, LK_INIT_LEVEL_EARLIEST, LK_INIT_LEVEL_THREADING - 1);

  arch_mp_init_percpu();

  dprintf(INFO, "RISCV: secondary cpu %u coming up\n", cpu_num);

  lk_secondary_cpu_entry();
}

zx_status_t riscv64_start_cpu(cpu_num_t cpu_num, uint32_t hart_id) {
  LTRACEF("cpu %u, hart %u\n", cpu_num, hart_id);

  DEBUG_ASSERT(cpu_num > 0 && cpu_num < SMP_MAX_CPUS && hart_id != riscv64_boot_hart_id());

  vaddr_t sp = 0;
  zx_status_t status = riscv64_create_secondary_stack(cpu_num, &sp);
  if (status != ZX_OK) {
    return status;
  }
  DEBUG_ASSERT(is_kernel_address(sp));

  // Issue memory barrier before starting to ensure previous stores will be visible to new cpu.
  arch::ThreadMemoryBarrier();

  // Compute the entry point in physical address.
  uintptr_t kernel_secondary_entry_paddr =
      reinterpret_cast<uintptr_t>(&riscv64_secondary_entry_asm);
  kernel_secondary_entry_paddr +=
      get_kernel_base_phys() - reinterpret_cast<uintptr_t>(__executable_start);

  LTRACEF("physical address of entry point at %p is %#lx\n", &riscv64_secondary_entry_asm,
          kernel_secondary_entry_paddr);

  // Tell SBI to start the secondary cpu.
  dprintf(INFO, "RISCV: Starting cpu %u, hart id %u\n", cpu_num, hart_id);
  arch::RiscvSbiError ret = sbi_hart_start(hart_id, kernel_secondary_entry_paddr, sp).error;
  if (ret != arch::RiscvSbiError::kSuccess) {
    // start failed, free the stack
    KERNEL_OOPS("RISCV: failed to start secondary cpu, SBI error %ld\n", static_cast<long>(ret));
    status = riscv64_free_secondary_stack(cpu_num);
    DEBUG_ASSERT(status == ZX_OK);
    return ZX_ERR_INTERNAL;
  }
  return ZX_OK;
}
