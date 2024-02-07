// Copyright 2023 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_FEATURE_H_
#define ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_FEATURE_H_

#include <lib/arch/riscv64/feature.h>
#include <stdbool.h>
#include <stdint.h>

// RISC-V feature bitset.
extern arch::RiscvFeatures gRiscvFeatures;

extern uint32_t riscv_cboz_size;
extern uint32_t riscv_cbom_size;

// The length of the vector registers in bytes. A value of 0 corresponds to the
// hardware not supporting vectors.
extern uint64_t riscv_vlenb;

void riscv64_feature_early_init();
void riscv64_feature_init();

#endif  // ZIRCON_KERNEL_ARCH_RISCV64_INCLUDE_ARCH_RISCV64_FEATURE_H_
