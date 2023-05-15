// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT
#include <assert.h>
#include <lib/arch/intrin.h>
#include <platform.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <arch/mp.h>
#include <arch/ops.h>
#include <arch/riscv64/sbi.h>
#include <dev/interrupt.h>
#include <kernel/event.h>

#define LOCAL_TRACE 0

// total number of detected cpus, accessed via arch_max_num_cpus()
uint riscv64_num_cpus = 1;

namespace {

// mapping of cpu -> hart
// kept separate from the percpu array for speed purposes
uint32_t cpu_to_hart_map[SMP_MAX_CPUS] = {0};

// per cpu structures, each cpu will point to theirs using the fixed register
riscv64_percpu riscv64_percpu_array[SMP_MAX_CPUS];

// list of IPIs queued per cpu
ktl::atomic<uint32_t> ipi_data[SMP_MAX_CPUS];

}  // anonymous namespace

// software triggered exceptions, used for cross-cpu calls
void riscv64_software_exception() {
  uint current_hart = riscv64_curr_hart_id();

  // Clear the IPI by clearing the pending software IPI bit.
  riscv64_csr_clear(RISCV64_CSR_SIP, RISCV64_CSR_SIP_SIP);

  rmb();
  uint32_t reason = ipi_data[current_hart].exchange(0);
  LTRACEF("current_hart %u reason %#x\n", current_hart, reason);

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
    panic("unimplemented MP_IPI_INTERRUPT\n");
    reason &= ~(1u << MP_IPI_INTERRUPT);
  }
  if (reason & (1u << MP_IPI_HALT)) {
    panic("unimplemented MP_IPI_HALT\n");
    reason &= ~(1u << MP_IPI_HALT);
  }

  if (unlikely(reason)) {
    panic("RISCV: unhandled ipi cause %#x, hartid %#x\n", reason, current_hart);
  }
}

void arch_prepare_current_cpu_idle_state(bool idle) {
  // no-op
}

void arch_mp_reschedule(cpu_mask_t mask) {
  arch_mp_send_ipi(MP_IPI_TARGET_MASK, mask, MP_IPI_RESCHEDULE);
}

void arch_mp_send_ipi(mp_ipi_target_t target, cpu_mask_t mask, mp_ipi_t ipi) {
  LTRACEF("target %d mask %#x, ipi %d\n", target, mask, ipi);

  arch::HartMask hart_mask = 0;
  arch::HartMaskBase hart_mask_base = 0;

  // translate the high level target + mask mechanism into just a mask
  switch (target) {
    case MP_IPI_TARGET_ALL:
      hart_mask = -1;
      break;
    case MP_IPI_TARGET_ALL_BUT_LOCAL:
      hart_mask = ~(1UL << riscv64_curr_hart_id());
      break;
    case MP_IPI_TARGET_MASK:
      for (uint cpu = 0; cpu < SMP_MAX_CPUS && mask; cpu++, mask >>= 1) {
        if (mask & 1) {
          uint64_t hart = cpu_to_hart_map[cpu];
          LTRACEF("cpu %u hart %lu mask %#x\n", cpu, hart, mask);

          // record a pending hart to notify
          hart_mask |= (1ul << hart);

          // set the ipi_data based on the incoming ipi
          ipi_data[hart].fetch_or(1u << ipi);
        }
      }
      break;
  }

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
