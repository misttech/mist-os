// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_LIB_LOG_CPP_INCLUDE_DFV2_WLAN_DRIVERS_TESTING_TEST_HELPERS_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_LIB_LOG_CPP_INCLUDE_DFV2_WLAN_DRIVERS_TESTING_TEST_HELPERS_H_

#include <lib/async-loop/cpp/loop.h>
#include <lib/driver/logging/cpp/logger.h>

#include <memory>

namespace wlan::drivers::log::testing {

// In DFv2, drivers normally set a global logging instance to be used by the driver logging macros.
// In unit tests that only test functions internal to the driver (and do not instantiate the whole
// driver with a `DriverBase::Start` call), tests must manually create an fdf::Logger.
//
// This class wraps the logic to create fdf::Logger instances and set the global instance of the
// logger so that unit tests can use the driver logging macros.
struct UnitTestLogContext {
  explicit UnitTestLogContext(std::string name);
  ~UnitTestLogContext();

  std::unique_ptr<async::Loop> loop_;
  std::unique_ptr<fdf::Logger> logger_;
};

}  // namespace wlan::drivers::log::testing

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_LIB_LOG_CPP_INCLUDE_DFV2_WLAN_DRIVERS_TESTING_TEST_HELPERS_H_
