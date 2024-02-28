// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_TESTING_CPP_FIXTURES_GTEST_FIXTURE_H_
#define LIB_DRIVER_TESTING_CPP_FIXTURES_GTEST_FIXTURE_H_

#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/testing/cpp/fixtures/base_fixture.h>

#include <gtest/gtest.h>

namespace fdf_testing {

// A gtest based fixture that can be used to write driver unit tests. It provides configuration
// through a class given in the template argument. See |BaseDriverTestFixture| for more
// information about the Configuration class.
template <class Configuration>
class DriverTestFixture : public BaseDriverTestFixture<Configuration>, public ::testing::Test {};

// The Configuration must be given a non-void DriverType, but not all tests need access to the
// driver type.
class EmptyDriverType {};

// A class that can be used in the Configuration's EnvironmentType if no environment customization
// is needed. Provides a minimal compat server.
class MinimalEnvironment {
 public:
  static std::unique_ptr<MinimalEnvironment> CreateAndInitialize(
      fdf::OutgoingDirectory& to_driver_vfs) {
    auto env = std::make_unique<MinimalEnvironment>();
    env->device_server_.Init(component::kDefaultInstance, "root");
    EXPECT_EQ(ZX_OK, env->device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(),
                                               &to_driver_vfs));

    return env;
  }

 private:
  compat::DeviceServer device_server_;
};

}  // namespace fdf_testing

#endif  // LIB_DRIVER_TESTING_CPP_FIXTURES_GTEST_FIXTURE_H_
