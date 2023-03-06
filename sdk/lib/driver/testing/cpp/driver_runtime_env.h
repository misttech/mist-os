// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_TESTING_CPP_DRIVER_RUNTIME_ENV_H_
#define LIB_DRIVER_TESTING_CPP_DRIVER_RUNTIME_ENV_H_

#include <lib/fdf/env.h>
#include <zircon/assert.h>
#include <zircon/status.h>

namespace fdf_testing {

// |DriverRuntimeEnv| is a RAII wrapper over the managed driver runtime environment. By creating
// this class the driver runtime environment will initialize, and by destroying it the runtime
// will reset. This must appear first in a test's fields, before any dispatchers.
// Starting the driver runtime environment will start an initial managed thread to handle
// fdf dispatchers.
class DriverRuntimeEnv {
 public:
  // Starts the driver runtime environment. If the env fails to start, this will throw an assert.
  DriverRuntimeEnv() {
    zx_status_t status = fdf_env_start();
    ZX_ASSERT_MSG(ZX_OK == status, "Failed to initialize driver runtime env: %s",
                  zx_status_get_string(status));
  }
  // Resets the driver runtime environment.
  ~DriverRuntimeEnv() { fdf_env_reset(); }
};

}  // namespace fdf_testing

#endif  // LIB_DRIVER_TESTING_CPP_DRIVER_RUNTIME_ENV_H_
