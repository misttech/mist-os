// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_TESTING_CPP_DRIVER_LIFECYCLE_H_
#define LIB_DRIVER_TESTING_CPP_DRIVER_LIFECYCLE_H_

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/runtime/testing/cpp/dispatcher.h>
#include <lib/driver/symbols/symbols.h>

// This is the exported driver lifecycle symbol that the driver framework looks for.
// NOLINTNEXTLINE(bugprone-reserved-identifier)
extern "C" const DriverLifecycle __fuchsia_driver_lifecycle__;

namespace fdf_testing {

using OpaqueDriverPtr = void*;

// Start a driver using the DriverLifecycle symbol's start hook. This is optional to use, the test
// can also construct and start the driver on its own if it wants to do so.
//
// This MUST be called from the main test thread.
zx::result<OpaqueDriverPtr> StartDriver(
    fdf::DriverStartArgs start_args, fdf::TestSynchronizedDispatcher& driver_dispatcher,
    const DriverLifecycle& driver_lifecycle_symbol = __fuchsia_driver_lifecycle__);

// Templated version of |StartDriver| which will return the requested driver type.
template <typename Driver>
zx::result<Driver*> StartDriver(
    fdf::DriverStartArgs start_args, fdf::TestSynchronizedDispatcher& driver_dispatcher,
    const DriverLifecycle& driver_lifecycle_symbol = __fuchsia_driver_lifecycle__) {
  zx::result result =
      StartDriver(std::move(start_args), driver_dispatcher, driver_lifecycle_symbol);
  if (result.is_error()) {
    return result.take_error();
  }

  return zx::ok(static_cast<Driver*>(result.value()));
}

// Initiates the teardown of the driver and the driver dispatcher. Teardown consist of using the
// prepare_stop hook, waiting for the completion, and calling the stop lifecycle hook.
//
// This MUST be called from the main test thread.
zx::result<> TeardownDriver(
    OpaqueDriverPtr driver, fdf::TestSynchronizedDispatcher& driver_dispatcher,
    const DriverLifecycle& driver_lifecycle_symbol = __fuchsia_driver_lifecycle__);

}  // namespace fdf_testing

#endif  // LIB_DRIVER_TESTING_CPP_DRIVER_LIFECYCLE_H_
