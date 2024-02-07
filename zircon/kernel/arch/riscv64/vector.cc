// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "arch/riscv64/vector.h"

#include <lib/arch/riscv64/feature.h>
#include <stdint.h>
#include <zircon/compiler.h>

#include <arch/riscv64/feature.h>
#include <kernel/thread.h>
#include <ktl/optional.h>

// Save the current on-cpu vector state to the thread. Takes as an argument
// the current HW status in the sstatus register.
void riscv64_thread_vector_save(Thread* thread, Riscv64VectorStatus status) {
  DEBUG_ASSERT(gRiscvFeatures[arch::RiscvFeature::kVector]);

  switch (status) {
    case Riscv64VectorStatus::DIRTY:
      DEBUG_ASSERT(thread->arch().vector_state.v);

      // The hardware state is dirty, save the old state.
      riscv64_vector_save(&thread->arch().vector_state);

      // Record that this thread has modified the state and will need to restore it
      // from now on out on every context switch.
      thread->arch().vector_dirty = true;
      break;

    // These three states means the thread didn't modify the state, so do not
    // write anything back.
    case Riscv64VectorStatus::INITIAL:
      // The old thread has the initial zeroed state.
    case Riscv64VectorStatus::CLEAN:
      // The old thread has some valid state loaded, but it didn't modify it.
      break;
    case Riscv64VectorStatus::OFF:
      // We currently leave vector enabled on all the time, so we should never
      // get this state.
      panic(
          "riscv context switch: Vector registers were disabled during context switch: sstatus.vs %#lx\n",
          static_cast<unsigned long>(status));
  }
}

// Restores the vector state to hardware from the thread passed in. current_state_initial
// should hold whether or not the sstatus.fs bits are currently RISCV64_CSR_SSTATUS_VS_INITIAL,
// as an optimization to avoid re-reading sstatus any more than necessary.
void riscv64_thread_vector_restore(const Thread* t, Riscv64VectorStatus status) {
  DEBUG_ASSERT(gRiscvFeatures[arch::RiscvFeature::kVector]);

  // Restore the state from the new thread
  if (t->arch().vector_dirty) {
    DEBUG_ASSERT(t->arch().vector_state.v);
    riscv64_vector_restore(&t->arch().vector_state);

    // Set the vector hardware state to clean
    riscv64_csr_clear(RISCV64_CSR_SSTATUS, RISCV64_CSR_SSTATUS_VS_MASK);
    riscv64_csr_set(RISCV64_CSR_SSTATUS, RISCV64_CSR_SSTATUS_VS_CLEAN);
  } else if (status != Riscv64VectorStatus::INITIAL) {
    // Zero the state of the vector unit if it currently is known to have something other
    // than the initial state loaded.
    riscv64_vector_zero();

    // Set the vector hardware state to clean
    riscv64_csr_clear(RISCV64_CSR_SSTATUS, RISCV64_CSR_SSTATUS_VS_MASK);
    riscv64_csr_set(RISCV64_CSR_SSTATUS, RISCV64_CSR_SSTATUS_VS_INITIAL);
  } else {  // thread does not have dirty state and vector state == INITIAL
    // The old vector hardware should have the initial state here and we didn't reset it
    // so we should still be in the initial state.
    DEBUG_ASSERT(status == Riscv64VectorStatus::INITIAL);
  }
}

ktl::optional<uint64_t> riscv64_vlmax(uint64_t vtype) {
  uint64_t value = 8 * riscv_vlenb;  // VLEN

  // This computes VLEN / SEW
  const uint64_t vsew = (vtype & RISCV64_CSR_VTYPE_VSEW_MASK) >> RISCV64_CSR_VTYPE_VSEW_SHIFT;
  switch (vsew) {
    case 0b000:  // SEW = 8
      value /= 8;
      break;
    case 0b001:  // SEW = 16
      value /= 16;
      break;
    case 0b010:  // SEW = 32
      value /= 32;
      break;
    case 0b011:  // SEW = 64
      value /= 64;
      break;
    default:  // SEW = reserved
      return {};
  }

  uint64_t vlmul = (vtype & RISCV64_CSR_VTYPE_VLMUL_MASK) >> RISCV64_CSR_VTYPE_VLMUL_SHIFT;
  if (vlmul == 0b000) {         // LMUL = 1
  } else if (vlmul == 0b001) {  // LMUL = 2
    value *= 2;                 //
  } else if (vlmul == 0b010) {  // LMUL = 4
    value *= 4;                 //
  } else if (vlmul == 0b011) {  // LMUL = 8
    value *= 8;                 //
  } else if (vlmul == 0b100) {  // LMUL = reserved
    return {};                  //
  } else if (vlmul == 0b101) {  // LMUL = 1/8
    value /= 8;                 //
  } else if (vlmul == 0b110) {  // LMUL = 1/4
    value /= 4;                 //
  } else if (vlmul == 0b111) {  // LMUL = 1/2
    value /= 2;                 //
  } else {
    __UNREACHABLE;
  }

  // If the value is now zero, we had a divisor that was too large and thus an
  // invalid (SEW, LMUL) pair.
  return value == 0 ? ktl::nullopt : ktl::make_optional(value);
}
