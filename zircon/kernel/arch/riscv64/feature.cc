// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include "arch/riscv64/feature.h"

#include <debug.h>
#include <stdint.h>

// Detected CPU features
//
// For now, assume the features are present as they are on QEMU.
bool riscv_feature_svpbmt = true;

void riscv64_feature_early_init() {}

void riscv64_feature_init() {
  if (riscv_feature_svpbmt) {
    dprintf(INFO, "RISCV: feature svpbmt\n");
  }
}
