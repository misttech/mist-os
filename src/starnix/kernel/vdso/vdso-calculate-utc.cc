// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/affine/ratio.h>

#include "src/starnix/kernel/vdso/vdso-calculate-time.h"
#include "src/starnix/kernel/vdso/vdso-platform.h"

// This is in its own source file so it can be unit tested.
int64_t calculate_utc_time_nsec() {
  int64_t reference_time = calculate_monotonic_time_nsec();

  // Boot time to utc transform is read from vvar_data. The data is protected by a seqlock, and so
  // a seqlock reader is implemented
  // Check that the state of the seqlock shows that the transform is not being updated
  uint64_t seq_num1 = vvar.seq_num.load(std::memory_order_acquire);
  if (seq_num1 & 1) {
    // Cannot read, because a write is in progress
    return kUtcInvalid;
  }
  int64_t boot_to_utc_reference_offset =
      vvar.boot_to_utc_reference_offset.load(std::memory_order_acquire);
  int64_t boot_to_utc_synthetic_offset =
      vvar.boot_to_utc_synthetic_offset.load(std::memory_order_acquire);
  uint32_t boot_to_utc_reference_ticks =
      vvar.boot_to_utc_reference_ticks.load(std::memory_order_acquire);
  uint32_t boot_to_utc_synthetic_ticks =
      vvar.boot_to_utc_synthetic_ticks.load(std::memory_order_acquire);
  // Check that the state of the seqlock has not changed while reading the transform
  uint64_t seq_num2 = vvar.seq_num.load(std::memory_order_acquire);
  if (seq_num1 != seq_num2) {
    // Data has been updated during the reading of it, so is invalid
    return kUtcInvalid;
  }

  affine::Ratio boot_to_utc_ratio(boot_to_utc_synthetic_ticks, boot_to_utc_reference_ticks);
  return boot_to_utc_ratio.Scale(reference_time - boot_to_utc_reference_offset) +
         boot_to_utc_synthetic_offset;
}
