// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_KERNEL_VDSO_VVAR_DATA_H_
#define SRC_STARNIX_KERNEL_VDSO_VVAR_DATA_H_

#include <stdint.h>

#include "src/starnix/lib/linux_uapi/bindgen_atomics.h"

#ifdef __cplusplus
extern "C" {
#endif
// This struct contains the data that is initialized by Starnix before any process is launched.
// This struct can be written to by the Starnix kernel, but is read only from the vDSO code's
// perspective.
struct vvar_data {
  // Offset for converting from the raw system timer to zx_ticks_t
  StdAtomicI64 raw_ticks_to_ticks_offset;

  // Ratio which relates ticks (zx_ticks_get) to clock monotonic (zx_clock_get_monotonic).
  // Specifically...
  //
  // ClockMono(ticks) = (ticks * N) / D
  //
  StdAtomicU32 ticks_to_mono_numerator;
  StdAtomicU32 ticks_to_mono_denominator;
};
#ifdef __cplusplus
}
#endif

#endif  // SRC_STARNIX_KERNEL_VDSO_VVAR_DATA_H_
