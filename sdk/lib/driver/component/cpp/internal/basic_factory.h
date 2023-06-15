// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_INTERNAL_BASIC_FACTORY_H_
#define LIB_DRIVER_COMPONENT_CPP_INTERNAL_BASIC_FACTORY_H_

#include <lib/driver/component/cpp/driver_base.h>

namespace fdf_internal {

// This is the default Factory that is used to Create a Driver of type |Driver|, that inherits the
// |DriverBase| class. |Driver| must implement a constructor with the following
// signature and forward said parameters to the |DriverBase| base class:
//
//   T(DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher);
template <typename Driver>
class BasicFactory {
  static_assert(std::is_base_of_v<fdf::DriverBase, Driver>,
                "Driver has to inherit from DriverBase");
  static_assert(
      std::is_constructible_v<Driver, fdf::DriverStartArgs, fdf::UnownedSynchronizedDispatcher>,
      "Driver must contain a constructor with the signature '(fdf::DriverStartArgs, "
      "fdf::UnownedSynchronizedDispatcher)' in order to be used with the BasicFactory.");

 public:
  static void CreateDriver(fdf::DriverStartArgs start_args,
                           fdf::UnownedSynchronizedDispatcher driver_dispatcher,
                           fdf::StartCompleter completer) {
    auto driver = std::make_unique<Driver>(std::move(start_args), std::move(driver_dispatcher));
    fdf::DriverBase* driver_ptr = driver.get();
    completer.set_driver(std::move(driver));
    driver_ptr->Start(std::move(completer));
  }
};

}  // namespace fdf_internal

#endif  // LIB_DRIVER_COMPONENT_CPP_INTERNAL_BASIC_FACTORY_H_
