// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_BASIC_FACTORY_H_
#define LIB_DRIVER_COMPONENT_CPP_BASIC_FACTORY_H_

#include <lib/driver/component/cpp/driver_base.h>

namespace fdf {

// This is the default Factory that is used to Create a Driver of type |Driver|, that inherits the
// |DriverBase| class. |Driver| must implement a constructor with the following
// signature and forward said parameters to the |DriverBase| base class:
//
//   T(DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);
template <typename Driver>
class BasicFactory {
  static_assert(std::is_base_of_v<DriverBase, Driver>, "Driver has to inherit from DriverBase");
  static_assert(
      std::is_constructible_v<Driver, DriverStartArgs, fdf::UnownedSynchronizedDispatcher>,
      "Driver must contain a constructor with the signature '(fdf::DriverStartArgs, "
      "fdf::UnownedSynchronizedDispatcher)' in order to be used with the BasicFactory.");

 public:
  static zx::result<std::unique_ptr<DriverBase>> CreateDriver(
      DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher) {
    std::unique_ptr<DriverBase> driver =
        std::make_unique<Driver>(std::move(start_args), std::move(driver_dispatcher));
    auto result = driver->Start();
    if (result.is_error()) {
      FDF_LOGL(WARNING, driver->logger(), "Failed to Start driver: %s", result.status_string());
      return result.take_error();
    }

    return zx::ok(std::move(driver));
  }
};

}  // namespace fdf

#endif  // LIB_DRIVER_COMPONENT_CPP_BASIC_FACTORY_H_
