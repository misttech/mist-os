// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_TESTS_INTEGRATION_DRIVER_REGISTRY_H_
#define SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_TESTS_INTEGRATION_DRIVER_REGISTRY_H_

#include <fidl/fuchsia.driver.development/cpp/fidl.h>
#include <fidl/fuchsia.driver.registrar/cpp/wire.h>
#include <glob.h>
#include <lib/component/incoming/cpp/protocol.h>

#include <gtest/gtest.h>

class RegisteredTestDriver {
 public:
  void Init() {
    auto registrar = component::Connect<fuchsia_driver_registrar::DriverRegistrar>();

    ASSERT_TRUE(registrar.is_ok());

    registrar_client_ = fidl::WireSyncClient(std::move(*registrar));

    {
      auto result = registrar_client_->Register(fidl::StringView::FromExternal(
          std::string("fuchsia-pkg://fuchsia.com/msd-arm-mali-integration-tests#meta/") +
          GetTestDriverSuffix()));

      ASSERT_TRUE(result.ok()) << result.status_string();
      ASSERT_FALSE(result->is_error()) << result->error_value();
    }

    {
      auto result = registrar_client_->Register(fidl::StringView::FromExternal(
          std::string("fuchsia-pkg://fuchsia.com/msd-arm-mali-integration-tests#meta/") +
          GetRebindDriverSuffix()));

      ASSERT_TRUE(result.ok()) << result.status_string();
      ASSERT_FALSE(result->is_error()) << result->error_value();
    }
  }

  // TODO(https://fxbug.dev/42075799): Unify rebind and production drivers.
  const char* GetRebindDriverSuffix() { return "msd_arm_rebind.cm"; }
  const char* GetTestDriverSuffix() { return "msd_arm_test.cm"; }

  std::optional<std::string> GetParentTopologicalPath() {
    // TODO(https://fxbug.dev/42078129): Avoid hardcoding this path.
    // Find any aml-gpu-composite device. 05 is PDEV_VID_AMLOGIC, and 17 is
    // PDEV_DID_AMLOGIC_MALI_INIT. The PID depends on the board.
    glob_t glob_val;
    int glob_result = glob("/dev/sys/platform/05:*:17/aml-gpu-composite/aml-gpu/mali-composite", 0,
                           nullptr, &glob_val);
    if (glob_result != 0) {
      EXPECT_EQ(0, glob_result) << "Attempting to find GPU device failed";
      return {};
    }
    std::string first_path{glob_val.gl_pathv[0]};
    globfree(&glob_val);
    return first_path;
  }

 private:
  fidl::WireSyncClient<fuchsia_driver_registrar::DriverRegistrar> registrar_client_;
};

#endif  // SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_TESTS_INTEGRATION_DRIVER_REGISTRY_H_
