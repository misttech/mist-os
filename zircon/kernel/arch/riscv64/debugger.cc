// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <string.h>
#include <sys/types.h>
#include <trace.h>
#include <zircon/errors.h>
#include <zircon/syscalls/debug.h>
#include <zircon/types.h>

#include <arch/debugger.h>
#include <arch/regs.h>
#include <arch/riscv64.h>
#include <kernel/thread.h>
#include <kernel/thread_lock.h>

#define LOCAL_TRACE 0

zx_status_t arch_get_general_regs(Thread* thread, zx_thread_state_general_regs_t* out) {
  LTRACEF("thread %p out %p\n", thread, out);

  Guard<MonitoredSpinLock, IrqSave> thread_lock_guard{ThreadLock::Get(), SOURCE_TAG};

  DEBUG_ASSERT(thread->IsUserStateSavedLocked());

  // Punt if registers aren't available. E.g.,
  // TODO(https://fxbug.dev/30521): Registers aren't available in synthetic exceptions.
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

  Guard<MonitoredSpinLock, IrqSave> thread_lock_guard{ThreadLock::Get(), SOURCE_TAG};

  DEBUG_ASSERT(thread->IsUserStateSavedLocked());

  // Punt if registers aren't available. E.g.,
  // TODO(https://fxbug.dev/30521): Registers aren't available in synthetic exceptions.
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

  Guard<MonitoredSpinLock, IrqSave> thread_lock_guard{ThreadLock::Get(), SOURCE_TAG};

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

  Guard<MonitoredSpinLock, IrqSave> thread_lock_guard{ThreadLock::Get(), SOURCE_TAG};

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

// Currently no support for vector register state.
zx_status_t arch_get_vector_regs(Thread* thread, zx_thread_state_vector_regs_t* out) {
  LTRACEF("thread %p out %p\n", thread, out);

  *out = {};
  return ZX_OK;
}

zx_status_t arch_set_vector_regs(Thread* thread, const zx_thread_state_vector_regs_t* in) {
  LTRACEF("thread %p in %p\n", thread, in);
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
