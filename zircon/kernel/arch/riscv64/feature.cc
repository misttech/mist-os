// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "arch/riscv64/feature.h"

#include <debug.h>
#include <stdint.h>
#include <pow2.h>
#include <assert.h>
#include <arch/defines.h>

// Detected CPU features
//
// For now, assume the features are present as they are on QEMU.
bool riscv_feature_cbom = true;
bool riscv_feature_cboz = true;
bool riscv_feature_svpbmt = true;

uint32_t riscv_cbom_size = 64;
uint32_t riscv_cboz_size = 64;

void riscv64_feature_early_init() {}

void riscv64_feature_init() {
  if (riscv_feature_cbom) {
    dprintf(INFO, "RISCV: feature cbom, size %#x\n", riscv_cbom_size);

    // Make sure the detected cbom size is usable.
    DEBUG_ASSERT(riscv_cbom_size > 0 && ispow2(riscv_cbom_size));
  }
  if (riscv_feature_cboz) {
    dprintf(INFO, "RISCV: feature cboz, size %#x\n", riscv_cboz_size);

    // Make sure the detected cboz size is usable.
    DEBUG_ASSERT(riscv_cboz_size > 0 && ispow2(riscv_cboz_size) && riscv_cboz_size < PAGE_SIZE);
  }
  if (riscv_feature_svpbmt) {
    dprintf(INFO, "RISCV: feature svpbmt\n");
  }
}
