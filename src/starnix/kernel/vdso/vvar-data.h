// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_KERNEL_VDSO_VVAR_DATA_H_
#define SRC_STARNIX_KERNEL_VDSO_VVAR_DATA_H_

// IMPORTANT NOTE: This code is used to help generate the Linux uapi Rust
// bindings.  Anyone modifying this code MUST run
// //src/starnix/lib/linux_uapi/generate.py to re-generate the bindings (and to
// ensure the binding generator still works) when they have finished.  Also note
// that the bindings may not change, which is fine - better to check to be on
// the safe side.

// LINT.IfChange

#include "src/starnix/lib/linux_uapi/bindgen_atomics.h"

#ifdef __cplusplus
extern "C" {
#endif

// This struct contains the data that is initialized by Starnix before any process is launched.
// This struct can be written to by the Starnix kernel, but is read only from the vDSO code's
// perspective.
struct vvar_data {
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

#ifdef __cplusplus
}
#endif

// Note: ThenChange only accepts one file as a parameter - this triggers the
// warnings, but all of the generated files should be changed.
// LINT.ThenChange(//src/starnix/lib/linux_uapi/src/x86_64.rs)

#endif  // SRC_STARNIX_KERNEL_VDSO_VVAR_DATA_H_
