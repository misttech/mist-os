// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_KERNEL_VDSO_VDSO_CALCULATE_MONOTONIC_H_
#define SRC_STARNIX_KERNEL_VDSO_VDSO_CALCULATE_MONOTONIC_H_

#include <stdint.h>

// Returns monotonic time in nanoseconds.
// This should be equivalent to calling zx_clock_get_monotonic, however the result may
// differ slightly.
// TODO(fxbug.dev/301234275): Change the computation to always match Zircon computation
int64_t calculate_monotonic_time_nsec();

#endif  // SRC_STARNIX_KERNEL_VDSO_VDSO_CALCULATE_MONOTONIC_H_
