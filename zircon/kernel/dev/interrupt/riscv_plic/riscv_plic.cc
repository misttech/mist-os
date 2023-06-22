// Copyright 2020 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <assert.h>
#include <bits.h>
#include <inttypes.h>
#include <lib/arch/intrin.h>
#include <lib/ktrace.h>
#include <lib/root_resource_filter.h>
#include <lib/zbi-format/driver-config.h>
#include <reg.h>
#include <string.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <arch/regs.h>
#include <arch/riscv64.h>
#include <arch/riscv64/mp.h>
#include <dev/interrupt.h>
#include <kernel/cpu.h>
#include <kernel/stats.h>
#include <kernel/thread.h>
#include <lk/init.h>
#include <pdev/interrupt.h>
#include <vm/physmap.h>
#include <vm/vm.h>

// HACK: Temporary workaround for the SiFive HiFive Unleashed which has a
// different calculation formulas than QEMU-virt:
// #define SIFIVE_HIFIVE_UNLEASHED_HACK

// TODO-rvbringup: have the offsets of each hart target be defined in ZBI from device tree
#ifdef SIFIVE_HIFIVE_UNLEASHED_HACK
#define PLIC_HART_IDX(hart) ((hart) ? (2 * (hart)) : ~0U)
#define PLIC_PRIORITY(plic_base, irq) (plic_base + 4 * (irq))
#else
#define PLIC_HART_IDX(hart) ((2 * (hart)) + 1)
#define PLIC_PRIORITY(plic_base, irq) (plic_base + 4 + 4 * (irq))
#endif

#define PLIC_PENDING(plic_base, irq) (plic_base + 0x1000 + (4 * ((irq) / 32)))
#define PLIC_ENABLE(plic_base, irq, hart) \
  (plic_base + 0x2000 + (0x80 * PLIC_HART_IDX(hart)) + (4 * ((irq) / 32)))
#define PLIC_THRESHOLD(plic_base, hart) (plic_base + 0x200000 + (0x1000 * PLIC_HART_IDX(hart)))
#define PLIC_COMPLETE(plic_base, hart) (plic_base + 0x200004 + (0x1000 * PLIC_HART_IDX(hart)))
#define PLIC_CLAIM(plic_base, hart) PLIC_COMPLETE(plic_base, hart)

#define LOCAL_TRACE 0

namespace {

// values read from zbi
vaddr_t plic_base = 0;
uint plic_max_int = 0;

bool plic_is_valid_interrupt(unsigned int vector, uint32_t flags) {
  return (vector < plic_max_int);
}

uint32_t plic_get_base_vector() { return 0; }

uint32_t plic_get_max_vector() { return plic_max_int; }

void plic_init_percpu_early() {}

void plic_enable_vector(unsigned int vector, uint32_t hart_id) {
  const uintptr_t plic_enable_reg = PLIC_ENABLE(plic_base, vector, hart_id);
  uint32_t val = readl(plic_enable_reg);
  val |= (1U << (vector % 32));
  writel(val, plic_enable_reg);
}

void plic_disable_vector(unsigned int vector, uint32_t hart_id) {
  const uintptr_t plic_enable_reg = PLIC_ENABLE(plic_base, vector, hart_id);
  uint32_t val = readl(plic_enable_reg);
  val &= ~(1U << (vector % 32));
  writel(val, plic_enable_reg);
}

zx_status_t plic_mask_interrupt(unsigned int vector) {
  LTRACEF("vector %u\n", vector);

  if (vector >= plic_max_int) {
    return ZX_ERR_INVALID_ARGS;
  }

  plic_disable_vector(vector, riscv64_boot_hart_id());

  return ZX_OK;
}

zx_status_t plic_unmask_interrupt(unsigned int vector) {
  LTRACEF("vector %u\n", vector);

  if (vector >= plic_max_int) {
    return ZX_ERR_INVALID_ARGS;
  }

  plic_enable_vector(vector, riscv64_boot_hart_id());

  return ZX_OK;
}

zx_status_t plic_deactivate_interrupt(unsigned int vector) {
  if (vector >= plic_max_int) {
    return ZX_ERR_INVALID_ARGS;
  }

  // TODO-rvbringup: investigate what this would do
  PANIC_UNIMPLEMENTED;

  return ZX_OK;
}

zx_status_t plic_configure_interrupt(unsigned int vector, enum interrupt_trigger_mode tm,
                                     enum interrupt_polarity pol) {
  LTRACEF("vector %u, trigger mode %d, polarity %d\n", vector, tm, pol);

  if (vector >= plic_max_int) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (pol != IRQ_POLARITY_ACTIVE_HIGH) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  return ZX_OK;
}

zx_status_t plic_get_interrupt_config(unsigned int vector, enum interrupt_trigger_mode* tm,
                                      enum interrupt_polarity* pol) {
  LTRACEF("vector %u\n", vector);

  if (vector >= plic_max_int) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (tm) {
    *tm = IRQ_TRIGGER_MODE_EDGE;
  }
  if (pol) {
    *pol = IRQ_POLARITY_ACTIVE_HIGH;
  }

  return ZX_OK;
}

unsigned int plic_remap_interrupt(unsigned int vector) {
  LTRACEF("vector %u\n", vector);
  return vector;
}

void plic_handle_irq(iframe_t* frame) {
  // get the current vector
  const uint32_t curr_hart_id = riscv64_curr_hart_id();
  const uint32_t vector = readl(PLIC_CLAIM(plic_base, curr_hart_id));
  LTRACEF_LEVEL(2, "vector %u\n", vector);

  DEBUG_ASSERT(curr_hart_id == riscv64_boot_hart_id());

  if (unlikely(vector == 0)) {
    // spurious
    return;
  }

  cpu_num_t cpu = arch_curr_cpu_num();

  // ktrace_tiny(TAG_IRQ_ENTER, (vector << 8) | cpu);

  LTRACEF_LEVEL(2, "cpu %u currthread %p vector %u pc %#" PRIxPTR "\n", cpu, Thread::Current::Get(),
                vector, (uintptr_t)frame->regs.pc);

  // deliver the interrupt
  pdev_invoke_int_if_present(vector);

  // EOI
  writel(vector, PLIC_COMPLETE(plic_base, curr_hart_id));

  LTRACEF_LEVEL(2, "cpu %u exit\n", cpu);

  // ktrace_tiny(TAG_IRQ_EXIT, (vector << 8) | cpu);
}

void plic_send_ipi(cpu_mask_t target, mp_ipi_t ipi) { PANIC_UNIMPLEMENTED; }

void plic_init_percpu() {
  const uint32_t curr_hart = riscv64_curr_hart_id();
  LTRACEF("hart %u\n", curr_hart);

  // mask all irqs on this cpu
  for (uint i = 1; i < plic_max_int; i++) {
    plic_disable_vector(i, curr_hart);
  }
}

void plic_shutdown() { PANIC_UNIMPLEMENTED; }

void plic_shutdown_cpu() {
  // Nothing to be done here on the secondary cpus.
  ASSERT(arch_curr_cpu_num() != BOOT_CPU_ID);
}

bool plic_msi_is_supported() { return false; }

bool plic_msi_supports_masking() { return false; }

void plic_msi_mask_unmask(const msi_block_t* block, uint msi_id, bool mask) { PANIC_UNIMPLEMENTED; }

zx_status_t plic_msi_alloc_block(uint requested_irqs, bool can_target_64bit, bool is_msix,
                                 msi_block_t* out_block) {
  PANIC_UNIMPLEMENTED;
}

void plic_msi_free_block(msi_block_t* block) { PANIC_UNIMPLEMENTED; }

void plic_msi_register_handler(const msi_block_t* block, uint msi_id, int_handler handler,
                               void* ctx) {
  PANIC_UNIMPLEMENTED;
}

const struct pdev_interrupt_ops plic_ops = {
    .mask = plic_mask_interrupt,
    .unmask = plic_unmask_interrupt,
    .deactivate = plic_deactivate_interrupt,
    .configure = plic_configure_interrupt,
    .get_config = plic_get_interrupt_config,
    .is_valid = plic_is_valid_interrupt,
    .get_base_vector = plic_get_base_vector,
    .get_max_vector = plic_get_max_vector,
    .remap = plic_remap_interrupt,
    .send_ipi = plic_send_ipi,
    .init_percpu_early = plic_init_percpu_early,
    .init_percpu = plic_init_percpu,
    .handle_irq = plic_handle_irq,
    .shutdown = plic_shutdown,
    .shutdown_cpu = plic_shutdown_cpu,
    .msi_is_supported = plic_msi_is_supported,
    .msi_supports_masking = plic_msi_supports_masking,
    .msi_mask_unmask = plic_msi_mask_unmask,
    .msi_alloc_block = plic_msi_alloc_block,
    .msi_free_block = plic_msi_free_block,
    .msi_register_handler = plic_msi_register_handler,
};

}  //  anonymous namespace

void PLICInitEarly(const zbi_dcfg_riscv_plic_driver_t& config) {
  ASSERT(config.mmio_phys);

  LTRACE_ENTRY;

  plic_base = (vaddr_t)paddr_to_physmap(config.mmio_phys);
  plic_max_int = config.num_irqs;
  ASSERT(plic_base && plic_max_int);

  pdev_register_interrupts(&plic_ops);

  // mask all irqs and set their priority to 1
  for (uint i = 1; i < plic_max_int; i++) {
    plic_disable_vector(i, riscv64_boot_hart_id());
    writel(1, PLIC_PRIORITY(plic_base, i));
  }

  // set global priority threshold to 0
  writel(0, PLIC_THRESHOLD(plic_base, riscv64_boot_hart_id()));

  LTRACE_EXIT;
}

void PLICInitLate() {}
