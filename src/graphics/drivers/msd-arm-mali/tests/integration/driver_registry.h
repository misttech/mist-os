// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_TESTS_INTEGRATION_DRIVER_REGISTRY_H_
#define SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_TESTS_INTEGRATION_DRIVER_REGISTRY_H_

#include <fidl/fuchsia.driver.registrar/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>

#include <gtest/gtest.h>

class RegisteredTestDriver {
 public:
  void Init() {
    auto registrar = component::Connect<fuchsia_driver_registrar::DriverRegistrar>();

    ASSERT_TRUE(registrar.is_ok());

    registrar_client_ = fidl::WireSyncClient(std::move(*registrar));

    {
      auto result = registrar_client_->Register(fuchsia_pkg::wire::PackageUrl{
          "fuchsia-pkg://fuchsia.com/msd-arm-mali-integration-tests#meta/libmsd_arm_test.cm"});

      ASSERT_TRUE(result.ok()) << result.status_string();
      ASSERT_FALSE(result->is_error()) << result->error_value();
    }

    {
      auto result = registrar_client_->Register(fuchsia_pkg::wire::PackageUrl{
          "fuchsia-pkg://fuchsia.com/msd-arm-mali-integration-tests#meta/libmsd_arm_rebind.cm"});

      ASSERT_TRUE(result.ok()) << result.status_string();
      ASSERT_FALSE(result->is_error()) << result->error_value();
    }
  }

 private:
  fidl::WireSyncClient<fuchsia_driver_registrar::DriverRegistrar> registrar_client_;
};

#endif  // SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_TESTS_INTEGRATION_DRIVER_REGISTRY_H_
