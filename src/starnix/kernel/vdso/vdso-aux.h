// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_KERNEL_VDSO_VDSO_AUX_H_
#define SRC_STARNIX_KERNEL_VDSO_VDSO_AUX_H_

#include <stdint.h>

#include "vvar-data.h"

constexpr int64_t NSEC_PER_SEC = 1'000'000'000;
extern "C" vvar_data vvar;
// Returns the raw ticks value, which is obtained by reading the hardware clock (the TSC in x64).
uint64_t get_raw_ticks();

#endif  // SRC_STARNIX_KERNEL_VDSO_VDSO_AUX_H_
