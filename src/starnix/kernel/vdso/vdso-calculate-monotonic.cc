// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vdso-calculate-monotonic.h"

#include <lib/affine/ratio.h>

#include "vdso-aux.h"

int64_t calculate_monotonic_time_nsec() {
  uint64_t raw_ticks = get_raw_ticks();
  uint64_t ticks = raw_ticks + vvar.raw_ticks_to_ticks_offset.load(std::memory_order_acquire);
  affine::Ratio ticks_to_mono_ratio(vvar.ticks_to_mono_numerator.load(std::memory_order_acquire),
                                    vvar.ticks_to_mono_denominator.load(std::memory_order_acquire));
  return ticks_to_mono_ratio.Scale(ticks);
}
