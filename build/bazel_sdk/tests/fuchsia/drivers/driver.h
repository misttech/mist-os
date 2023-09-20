// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef BUILD_BAZEL_SDK_TESTS_FUCHSIA_DRIVERS_DRIVER_H_
#define BUILD_BAZEL_SDK_TESTS_FUCHSIA_DRIVERS_DRIVER_H_

#include <lib/driver/component/cpp/driver_base.h>

namespace example_driver {

class ExampleDriver : public fdf::DriverBase {
 public:
  ExampleDriver(fdf::DriverStartArgs start_args,
                fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase("example-driver", std::move(start_args), std::move(driver_dispatcher)) {}
  virtual ~ExampleDriver() = default;

  zx::result<> Start() override;

 private:
};

}  // namespace example_driver

#endif  // BUILD_BAZEL_SDK_TESTS_FUCHSIA_DRIVERS_DRIVER_H_
