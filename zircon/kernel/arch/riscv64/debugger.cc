// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <lib/kconcurrent/chainlock_transaction.h>
#include <string.h>
#include <sys/types.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/syscalls/debug.h>
#include <zircon/types.h>

#include <cstdint>

#include <arch/debugger.h>
#include <arch/regs.h>
#include <arch/riscv64.h>
#include <arch/riscv64/feature.h>
#include <arch/riscv64/vector.h>
#include <kernel/thread.h>
#include <ktl/align.h>
#include <ktl/bit.h>

#include <ktl/enforce.h>

#define LOCAL_TRACE 0

zx_status_t arch_get_general_regs(Thread* thread, zx_thread_state_general_regs_t* out) {
  LTRACEF("thread %p out %p\n", thread, out);

  SingleChainLockGuard thread_guard{IrqSaveOption, thread->get_lock(),
                                    CLT_TAG("arch_get_general_regs")};

  DEBUG_ASSERT(thread->IsUserStateSavedLocked());

  // Punt if registers aren't available. E.g.,
  // TODO(https://fxbug.dev/42105394): Registers aren't available in synthetic exceptions.
  if (thread->arch().suspended_general_regs == nullptr) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  const iframe_t* in = thread->arch().suspended_general_regs;
  DEBUG_ASSERT(in);

  *out = in->regs;

  return ZX_OK;
}

zx_status_t arch_set_general_regs(Thread* thread, const zx_thread_state_general_regs_t* in) {
  LTRACEF("thread %p in %p\n", thread, in);

  SingleChainLockGuard thread_guard{IrqSaveOption, thread->get_lock(),
                                    CLT_TAG("arch_set_general_regs")};

  DEBUG_ASSERT(thread->IsUserStateSavedLocked());

  // Punt if registers aren't available. E.g.,
  // TODO(https://fxbug.dev/42105394): Registers aren't available in synthetic exceptions.
  if (thread->arch().suspended_general_regs == nullptr) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  iframe_t* out = thread->arch().suspended_general_regs;
  DEBUG_ASSERT(out);

  out->regs = *in;

  return ZX_OK;
}

zx_status_t arch_get_fp_regs(Thread* thread, zx_thread_state_fp_regs_t* out) {
  LTRACEF("thread %p out %p\n", thread, out);

  SingleChainLockGuard thread_guard{IrqSaveOption, thread->get_lock(), CLT_TAG("arch_get_fp_regs")};

  DEBUG_ASSERT(thread->IsUserStateSavedLocked());

  *out = {};

  const riscv64_fpu_state* in = &thread->arch().fpu_state;
  for (int i = 0; i < 32; i++) {
    out->q[i].low = in->f[i];
    out->q[i].high = UINT64_MAX;
  }
  out->fcsr = in->fcsr;

  return ZX_OK;
}

zx_status_t arch_set_fp_regs(Thread* thread, const zx_thread_state_fp_regs_t* in) {
  LTRACEF("thread %p in %p\n", thread, in);

  // Check that the input is valid. The high bits must be all 1s.
  for (size_t i = 0; i < 32; i++) {
    if (in->q[i].high != UINT64_MAX) {
      return ZX_ERR_INVALID_ARGS;
    }
  }

  SingleChainLockGuard thread_guard{IrqSaveOption, thread->get_lock(), CLT_TAG("arch_set_fp_regs")};

  DEBUG_ASSERT(thread->IsUserStateSavedLocked());

  riscv64_fpu_state* out = &thread->arch().fpu_state;
  for (size_t i = 0; i < 32; i++) {
    out->f[i] = in->q[i].low;
  }
  out->fcsr = in->fcsr;

  // Mark the state as dirty in case it hadn't already been touched. This will
  // force the context switch routine to load it on next switch.
  thread->arch().fpu_dirty = true;

  return ZX_OK;
}

zx_status_t arch_get_vector_regs(Thread* thread, zx_thread_state_vector_regs_t* out) {
  LTRACEF("thread %p out %p\n", thread, out);

  if (!gRiscvFeatures[arch::RiscvFeature::kVector]) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  SingleChainLockGuard thread_guard{IrqSaveOption, thread->get_lock(),
                                    CLT_TAG("arch_get_vector_regs")};

  DEBUG_ASSERT(thread->IsUserStateSavedLocked());
  *out = thread->arch().vector_state;
  return ZX_OK;
}

zx_status_t arch_set_vector_regs(Thread* thread, const zx_thread_state_vector_regs_t* in) {
  LTRACEF("thread %p in %p\n", thread, in);

  if (!gRiscvFeatures[arch::RiscvFeature::kVector]) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  // vcsr[63:3] is reserved-as-zero.
  bool vcsr_rsvd_are_zero = (in->vcsr >> 3) == 0;
  if (!vcsr_rsvd_are_zero) {
    return ZX_ERR_INVALID_ARGS;
  }

  ktl::optional<uint64_t> vlmax = riscv64_vlmax(in->vtype);
  if (!vlmax) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Setting only VILL in vtype and setting vl as zero is the canonical 'reset'
  // state. Invalid values outside of that pair will be rejected.
  if (in->vtype != RISCV64_CSR_VTYPE_VILL || in->vl != 0) {
    // vcsr[63] (VILL) should be zero outside of the canonical reset state, and
    // vtype[62:8] is reserved-as-zero.
    bool vtype_rsvd_are_zero = (in->vtype >> 8) == 0;
    uint64_t sew = (in->vtype & RISCV64_CSR_VTYPE_VSEW_MASK) >> RISCV64_CSR_VTYPE_VSEW_SHIFT;
    uint64_t lmul = (in->vtype & RISCV64_CSR_VTYPE_VLMUL_MASK) >> RISCV64_CSR_VTYPE_VLMUL_SHIFT;
    if (!vtype_rsvd_are_zero || sew >= 0b100 || lmul == 0b100) {  // Reserved values.
      return ZX_ERR_INVALID_ARGS;
    }

    if (!ktl::has_single_bit(in->vl) || in->vl > *vlmax) {
      return ZX_ERR_INVALID_ARGS;
    }
  }

  // VLMAX - 1 is the largest possible element index.
  if (in->vstart >= *vlmax) {
    return ZX_ERR_INVALID_ARGS;
  }

  SingleChainLockGuard thread_guard{IrqSaveOption, thread->get_lock(),
                                    CLT_TAG("arch_set_vector_regs")};

  DEBUG_ASSERT(thread->IsUserStateSavedLocked());

  thread->arch().vector_state = *in;

  // Mark the state as dirty in case it hadn't already been touched. This will
  // force the context switch routine to load it on next switch.
  thread->arch().vector_dirty = true;

  return ZX_OK;
}

// Currently no support for single step debugging.
zx_status_t arch_get_single_step(Thread* thread, zx_thread_state_single_step_t* out) {
  LTRACEF("thread %p out %p\n", thread, out);
  return ZX_ERR_NOT_SUPPORTED;
}

zx_status_t arch_set_single_step(Thread* thread, const zx_thread_state_single_step_t* in) {
  LTRACEF("thread %p in %p\n", thread, in);
  return ZX_ERR_NOT_SUPPORTED;
}

// Debug registers are basically zero sized, so it's a success to load/store them, but no
// behavioral changes.
zx_status_t arch_get_debug_regs(Thread* thread, zx_thread_state_debug_regs_t* out) {
  LTRACEF("thread %p out %p\n", thread, out);

  *out = {};

  return ZX_OK;
}

zx_status_t arch_set_debug_regs(Thread* thread, const zx_thread_state_debug_regs_t* in) {
  LTRACEF("thread %p in %p\n", thread, in);
  return ZX_OK;
}

uint8_t arch_get_hw_breakpoint_count() { return 0; }

uint8_t arch_get_hw_watchpoint_count() { return 0; }
