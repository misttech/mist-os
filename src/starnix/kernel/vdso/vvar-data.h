// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_KERNEL_VDSO_VVAR_DATA_H_
#define SRC_STARNIX_KERNEL_VDSO_VVAR_DATA_H_

#include <zircon/compiler.h>

#include "src/starnix/lib/linux_uapi/bindgen_atomics.h"

__BEGIN_CDECLS

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

  // Implements a seqlock
  StdAtomicU64 seq_num;

  // Linear transform which relates clock monotonic (zx_clock_get_monotonic) to utc time.
  // Specifically...
  //
  // utc(monotonic_time) =  (monotonic_time - mono_to_utc_reference_offset)
  //                        * mono_to_utc_synthetic_ticks
  //                        / mono_to_utc_reference_ticks
  //                        + mono_to_utc_synthetic_offset;
  //
  // This transform is protected by a seqlock, implemented using seq_num, to prevent
  // the vDSO reading the transform while it is being updated      .
  StdAtomicI64 mono_to_utc_reference_offset;
  StdAtomicI64 mono_to_utc_synthetic_offset;
  StdAtomicU32 mono_to_utc_reference_ticks;
  StdAtomicU32 mono_to_utc_synthetic_ticks;
};

__END_CDECLS

#endif  // SRC_STARNIX_KERNEL_VDSO_VVAR_DATA_H_
