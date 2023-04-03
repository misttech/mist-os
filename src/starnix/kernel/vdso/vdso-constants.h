// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_KERNEL_VDSO_VDSO_CONSTANTS_H_
#define SRC_STARNIX_KERNEL_VDSO_VDSO_CONSTANTS_H_

#define VDSO_CONSTANTS_ALIGN 8

// The manifest for the constants size is currently...
// + 2 32-bit integers
// |++ ticks to mono ratio (2)
// |
// + 1 64-bit integer
// | raw_ticks to ticks offset (1)
//
#define VDSO_CONSTANTS_SIZE ((2 * 4) + 8)

#ifndef __ASSEMBLER__

#include <stdint.h>

// This struct contains constants that are initialized by Starnix before any process is launched.
// From the vDSO code's perspective, they are read-only data that can never change.
#ifdef __cplusplus
extern "C"
#endif
    struct vdso_constants {
  // Offset for converting from the raw system timer to zx_ticks_t
  int64_t raw_ticks_to_ticks_offset;

  // Ratio which relates ticks (zx_ticks_get) to clock monotonic (zx_clock_get_monotonic).
  // Specifically...
  //
  // ClockMono(ticks) = (ticks * N) / D
  //
  uint32_t ticks_to_mono_numerator;
  uint32_t ticks_to_mono_denominator;
};

#ifdef __cplusplus

static_assert(VDSO_CONSTANTS_SIZE == sizeof(vdso_constants), "Need to adjust VDSO_CONSTANTS_SIZE");
static_assert(VDSO_CONSTANTS_ALIGN == alignof(vdso_constants),
              "Need to adjust VDSO_CONSTANTS_ALIGN");
#endif  // __cplusplus

#endif  // __ASSEMBLER__

#endif  // SRC_STARNIX_KERNEL_VDSO_VDSO_CONSTANTS_H_
