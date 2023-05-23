// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_RUNTIME_TESTING_CPP_INTERNAL_WAIT_FOR_H_
#define LIB_DRIVER_RUNTIME_TESTING_CPP_INTERNAL_WAIT_FOR_H_

#include <lib/fdf/cpp/dispatcher.h>
#include <lib/fit/function.h>
#include <lib/zx/result.h>

namespace fdf::internal {

// Waits for a condition to become true while running the driver dispatchers,
// unless the driver runtime is managing threads.
//
// Returns an OK result when:
//
// - The condition is true.
// - The driver runtime is managing any threads, in which case manual running
//   of the driver dispatchers is impossible.
//
// Propagates any other error from running the dispatchers.
zx::result<> CheckManagedThreadOrWaitUntil(fit::function<bool()> condition);

}  // namespace fdf::internal

#endif  // LIB_DRIVER_RUNTIME_TESTING_CPP_INTERNAL_WAIT_FOR_H_
