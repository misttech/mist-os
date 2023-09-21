// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_STARNIX_KERNEL_VDSO_VDSO_CALCULATE_UTC_H_
#define SRC_STARNIX_KERNEL_VDSO_VDSO_CALCULATE_UTC_H_

#include <stdint.h>

const int64_t UTC_INVALID = 0;

// Returns utc time in nanoseconds
int64_t calculate_utc_time_nsec();

#endif  // SRC_STARNIX_KERNEL_VDSO_VDSO_CALCULATE_UTC_H_
