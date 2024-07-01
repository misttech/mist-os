// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "arch/riscv64/feature.h"

#include <assert.h>
#include <debug.h>
#include <lib/arch/riscv64/feature.h>
#include <pow2.h>
#include <stdint.h>

#include <arch/defines.h>
#include <arch/riscv64.h>
#include <phys/handoff.h>

// Detected CPU features
arch::RiscvFeatures gRiscvFeatures;

uint32_t riscv_cbom_size = 64;
uint32_t riscv_cboz_size = 64;
uint64_t riscv_vlenb = 0;

void riscv64_feature_early_init() {
  gRiscvFeatures = gPhysHandoff->arch_handoff.cpu_features;

  if (gRiscvFeatures[arch::RiscvFeature::kVector]) {
    // We need vectors to have been enabled in order to read vlenb, but cannot
    // assume that they have been enabled at this point. Here is a good enough
    // place to initially turn the on as any.
    uint64_t sstatus_initial = riscv64_csr_read(RISCV64_CSR_SSTATUS);
    riscv64_csr_set(RISCV64_CSR_SSTATUS, RISCV64_CSR_SSTATUS_VS_INITIAL);
    riscv_vlenb = riscv64_csr_read(RISCV64_CSR_VLENB);

    // Current support is provisional and only for 16-byte vector registers,
    // the minimal possible length.
    if (riscv_vlenb != 16) {
      // Restore original sstatus.
      riscv64_csr_write(RISCV64_CSR_SSTATUS, sstatus_initial);
      gRiscvFeatures.Set(arch::RiscvFeature::kVector, false);
    }
  }
}

void riscv64_feature_init() {
  if (gRiscvFeatures[arch::RiscvFeature::kZicntr]) {
    dprintf(INFO, "RISCV: feature zicntr\n");
  }
  if (gRiscvFeatures[arch::RiscvFeature::kZicbom]) {
    dprintf(INFO, "RISCV: feature cbom, size %#x\n", riscv_cbom_size);

    // Make sure the detected cbom size is usable.
    DEBUG_ASSERT(riscv_cbom_size > 0 && ispow2(riscv_cbom_size));
  } else {
    riscv_cbom_size = 0;
  }
  if (gRiscvFeatures[arch::RiscvFeature::kZicboz]) {
    dprintf(INFO, "RISCV: feature cboz, size %#x\n", riscv_cboz_size);

    // Make sure the detected cboz size is usable.
    DEBUG_ASSERT(riscv_cboz_size > 0 && ispow2(riscv_cboz_size) && riscv_cboz_size < PAGE_SIZE);
  } else {
    riscv_cboz_size = 0;
  }
  if (gRiscvFeatures[arch::RiscvFeature::kSstc]) {
    dprintf(INFO, "RISCV: feature sstc\n");
  }
  if (gRiscvFeatures[arch::RiscvFeature::kSvpbmt]) {
    dprintf(INFO, "RISCV: feature svpbmt\n");
  }
  if (gRiscvFeatures[arch::RiscvFeature::kVector]) {
    dprintf(INFO, "RISCV: feature vector, register length = %#lx\n", riscv_vlenb);
  } else if (riscv_vlenb > 0) {
    dprintf(INFO, "RISCV: feature vector disabled; register length (%#lx) too large\n",
            riscv_vlenb);
  }
}
