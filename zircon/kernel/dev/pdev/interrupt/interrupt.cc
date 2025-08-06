// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/arch/intrin.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <kernel/spinlock.h>
#include <lk/init.h>
#include <pdev/interrupt.h>

#include <ktl/enforce.h>

namespace {

DECLARE_SINGLETON_SPINLOCK(pdev_lock);

struct int_handler_struct {
  interrupt_handler_t handler TA_GUARDED(pdev_lock::Get()) = nullptr;
  ktl::atomic<bool> permanent = false;
};

struct int_handler_struct int_handler_table[MAX_INTERRUPTS];

struct int_handler_struct* pdev_get_int_handler(interrupt_vector_t vector) {
  DEBUG_ASSERT(vector < MAX_INTERRUPTS);
  return &int_handler_table[vector];
}

zx_status_t register_int_handler_common(interrupt_vector_t vector, interrupt_handler_t handler,
                                        bool permanent) {
  if (!is_valid_interrupt(vector, 0)) {
    return ZX_ERR_INVALID_ARGS;
  }

  Guard<SpinLock, IrqSave> guard{pdev_lock::Get()};

  auto h = pdev_get_int_handler(vector);
  if ((handler && h->handler) || h->permanent.load(ktl::memory_order_relaxed)) {
    return ZX_ERR_ALREADY_BOUND;
  }
  h->handler = ktl::move(handler);
  h->permanent.store(permanent, ktl::memory_order_relaxed);

  return ZX_OK;
}

// By default most of these are empty stubs and the particular interrupt controller must override
// all of them.
const struct pdev_interrupt_ops default_ops = {
    .mask = [](interrupt_vector_t) { return ZX_ERR_NOT_SUPPORTED; },
    .unmask = [](interrupt_vector_t) { return ZX_ERR_NOT_SUPPORTED; },
    .deactivate = [](interrupt_vector_t) { return ZX_ERR_NOT_SUPPORTED; },
    .configure = [](interrupt_vector_t, interrupt_trigger_mode,
                    interrupt_polarity) { return ZX_ERR_NOT_SUPPORTED; },
    .get_config = [](interrupt_vector_t, interrupt_trigger_mode*,
                     interrupt_polarity*) { return ZX_ERR_NOT_SUPPORTED; },
    .set_affinity = [](interrupt_vector_t, cpu_mask_t) { return ZX_ERR_NOT_SUPPORTED; },
    .is_valid = [](interrupt_vector_t, uint32_t flags) { return false; },
    .get_base_vector = []() -> interrupt_vector_t { return 0; },
    .get_max_vector = []() -> interrupt_vector_t { return 0; },
    .remap = [](interrupt_vector_t) -> interrupt_vector_t { return 0; },
    .send_ipi = [](cpu_mask_t, mp_ipi) { return ZX_ERR_NOT_SUPPORTED; },
    .init_percpu_early = []() {},
    .init_percpu = []() {},
    .handle_irq = [](iframe_t*) {},
    .shutdown = []() {},
    .shutdown_cpu = []() {},
    .suspend_cpu = []() { return ZX_ERR_NOT_SUPPORTED; },
    .resume_cpu = []() { return ZX_ERR_NOT_SUPPORTED; },
    .msi_is_supported = []() { return false; },
    .msi_supports_masking = []() { return false; },
    .msi_mask_unmask = [](const msi_block_t*, uint, bool) {},
    .msi_alloc_block = [](uint, bool, bool, msi_block_t*) { return ZX_ERR_NOT_SUPPORTED; },
    .msi_free_block = [](msi_block_t*) {},
    .msi_register_handler = [](const msi_block_t*, uint, interrupt_handler_t) {}};

const struct pdev_interrupt_ops* intr_ops = &default_ops;

}  // anonymous namespace

zx_status_t register_int_handler(interrupt_vector_t vector, interrupt_handler_t handler) {
  return register_int_handler_common(vector, ktl::move(handler), false);
}

zx_status_t register_permanent_int_handler(interrupt_vector_t vector, interrupt_handler_t handler) {
  return register_int_handler_common(vector, ktl::move(handler), true);
}

bool pdev_invoke_int_if_present(interrupt_vector_t vector) {
  auto h = pdev_get_int_handler(vector);
  // Use a relaxed load as permanent handlers are never modified once set, and they are only set in
  // startup code, and so there is nothing to race with.
  if (h->permanent.load(ktl::memory_order_relaxed)) {
    // Once permanent is set to true we know that handler and arg are immutable and so it is safe
    // to read them without holding the lock.
    [&h]() TA_NO_THREAD_SAFETY_ANALYSIS {
      DEBUG_ASSERT(h->handler);
      h->handler();
    }();
    return true;
  }
  Guard<SpinLock, IrqSave> guard{pdev_lock::Get()};

  if (h->handler) {
    h->handler();
    return true;
  }
  return false;
}

zx_status_t mask_interrupt(interrupt_vector_t vector) { return intr_ops->mask(vector); }

zx_status_t unmask_interrupt(interrupt_vector_t vector) { return intr_ops->unmask(vector); }

zx_status_t deactivate_interrupt(interrupt_vector_t vector) { return intr_ops->deactivate(vector); }

zx_status_t configure_interrupt(interrupt_vector_t vector, enum interrupt_trigger_mode tm,
                                enum interrupt_polarity pol) {
  return intr_ops->configure(vector, tm, pol);
}

zx_status_t get_interrupt_config(interrupt_vector_t vector, enum interrupt_trigger_mode* tm,
                                 enum interrupt_polarity* pol) {
  return intr_ops->get_config(vector, tm, pol);
}

zx_status_t set_interrupt_affinity(interrupt_vector_t vector, cpu_mask_t mask) {
  return intr_ops->set_affinity(vector, mask);
}

uint32_t interrupt_get_base_vector() { return intr_ops->get_base_vector(); }

uint32_t interrupt_get_max_vector() { return intr_ops->get_max_vector(); }

bool is_valid_interrupt(interrupt_vector_t vector, uint32_t flags) {
  return intr_ops->is_valid(vector, flags);
}

interrupt_vector_t remap_interrupt(interrupt_vector_t vector) { return intr_ops->remap(vector); }

zx_status_t interrupt_send_ipi(cpu_mask_t target, mp_ipi ipi) {
  return intr_ops->send_ipi(target, ipi);
}

void interrupt_init_percpu_early() { intr_ops->init_percpu_early(); }
void interrupt_init_percpu() { intr_ops->init_percpu(); }

void platform_irq(iframe_t* frame) { intr_ops->handle_irq(frame); }

void pdev_register_interrupts(const struct pdev_interrupt_ops* ops) {
  // Assert that all of the ops are fulled in with at least a default hook.
  DEBUG_ASSERT(ops->mask);
  DEBUG_ASSERT(ops->unmask);
  DEBUG_ASSERT(ops->deactivate);
  DEBUG_ASSERT(ops->configure);
  DEBUG_ASSERT(ops->get_config);
  DEBUG_ASSERT(ops->set_affinity);
  DEBUG_ASSERT(ops->is_valid);
  DEBUG_ASSERT(ops->get_base_vector);
  DEBUG_ASSERT(ops->get_max_vector);
  DEBUG_ASSERT(ops->remap);
  DEBUG_ASSERT(ops->send_ipi);
  DEBUG_ASSERT(ops->init_percpu_early);
  DEBUG_ASSERT(ops->init_percpu);
  DEBUG_ASSERT(ops->handle_irq);
  DEBUG_ASSERT(ops->shutdown);
  DEBUG_ASSERT(ops->shutdown_cpu);
  DEBUG_ASSERT(ops->suspend_cpu);
  DEBUG_ASSERT(ops->resume_cpu);
  DEBUG_ASSERT(ops->msi_is_supported);
  DEBUG_ASSERT(ops->msi_supports_masking);
  DEBUG_ASSERT(ops->msi_mask_unmask);
  DEBUG_ASSERT(ops->msi_alloc_block);
  DEBUG_ASSERT(ops->msi_free_block);
  DEBUG_ASSERT(ops->msi_register_handler);

  intr_ops = ops;
  arch::ThreadMemoryBarrier();
}

void shutdown_interrupts() { intr_ops->shutdown(); }

void shutdown_interrupts_curr_cpu() { intr_ops->shutdown_cpu(); }

zx_status_t suspend_interrupts_curr_cpu() { return intr_ops->suspend_cpu(); }

zx_status_t resume_interrupts_curr_cpu() { return intr_ops->resume_cpu(); }

bool msi_is_supported() { return intr_ops->msi_is_supported(); }

bool msi_supports_masking() { return intr_ops->msi_supports_masking(); }

void msi_mask_unmask(const msi_block_t* block, uint msi_id, bool mask) {
  intr_ops->msi_mask_unmask(block, msi_id, mask);
}

zx_status_t msi_alloc_block(uint requested_irqs, bool can_target_64bit, bool is_msix,
                            msi_block_t* out_block) {
  return intr_ops->msi_alloc_block(requested_irqs, can_target_64bit, is_msix, out_block);
}

void msi_free_block(msi_block_t* block) { intr_ops->msi_free_block(block); }

void msi_register_handler(const msi_block_t* block, uint msi_id, interrupt_handler_t handler) {
  intr_ops->msi_register_handler(block, msi_id, ktl::move(handler));
}

namespace {

void interrupt_init_percpu_early_hook(uint level) { interrupt_init_percpu_early(); }

LK_INIT_HOOK_FLAGS(interrupt_init_percpu_early, interrupt_init_percpu_early_hook,
                   LK_INIT_LEVEL_PLATFORM_EARLY, LK_INIT_FLAG_SECONDARY_CPUS)
}  // namespace
