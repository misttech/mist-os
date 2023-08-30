// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_TESTS_INTEGRATION_DRIVER_REGISTRY_H_
#define SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_TESTS_INTEGRATION_DRIVER_REGISTRY_H_

#include <fidl/fuchsia.driver.development/cpp/fidl.h>
#include <fidl/fuchsia.driver.registrar/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>

#include <gtest/gtest.h>

class RegisteredTestDriver {
 public:
  void Init() {
    // Check whether DFv2 driver is loaded to determine whether to load a DFv2 test driver.
    {
      auto driver_development = component::Connect<fuchsia_driver_development::DriverDevelopment>();
      ASSERT_TRUE(driver_development.is_ok());
      auto development_client = fidl::SyncClient(std::move(*driver_development));

      auto endpoints = fidl::CreateEndpoints<fuchsia_driver_development::DriverInfoIterator>();
      ASSERT_TRUE(endpoints.is_ok());

      const char* kDFv2DriverManifestUrl =
          "fuchsia-pkg://fuchsia.com/msd-arm-mali-dfv2#meta/libmsd_arm_dfv2.cm";
      auto driver_info = development_client->GetDriverInfo(
          {{kDFv2DriverManifestUrl, "fuchsia-pkg://fuchsia.com/msd-arm-mali#meta/libmsd_arm.cm"},
           std::move(endpoints->server)});

      ASSERT_TRUE(driver_info.is_ok()) << driver_info.error_value();

      auto driver_info_client = fidl::SyncClient(std::move(endpoints->client));

      auto first_driver_info = driver_info_client->GetNext();
      ASSERT_TRUE(first_driver_info.is_ok());
      // Exactly one driver should be available, since the test doesn't work with 0 drivers, and
      // having both will clash because they have identical bind rules.
      ASSERT_EQ(first_driver_info->drivers().size(), 1u);
      fuchsia_driver_development::DriverInfo& driver = first_driver_info->drivers()[0];
      ASSERT_TRUE(driver.url().has_value());

      is_dfv2_ = driver.url() == kDFv2DriverManifestUrl;

      printf("Driver %s is DFv2: %d\n", driver.url()->c_str(), is_dfv2_);
    }

    auto registrar = component::Connect<fuchsia_driver_registrar::DriverRegistrar>();

    ASSERT_TRUE(registrar.is_ok());

    registrar_client_ = fidl::WireSyncClient(std::move(*registrar));

    {
      auto result =
          registrar_client_->Register(fuchsia_pkg::wire::PackageUrl{fidl::StringView::FromExternal(
              std::string("fuchsia-pkg://fuchsia.com/msd-arm-mali-integration-tests#meta/") +
              GetTestDriverSuffix())});

      ASSERT_TRUE(result.ok()) << result.status_string();
      ASSERT_FALSE(result->is_error()) << result->error_value();
    }

    {
      auto result =
          registrar_client_->Register(fuchsia_pkg::wire::PackageUrl{fidl::StringView::FromExternal(
              std::string("fuchsia-pkg://fuchsia.com/msd-arm-mali-integration-tests#meta/") +
              GetRebindDriverSuffix())});

      ASSERT_TRUE(result.ok()) << result.status_string();
      ASSERT_FALSE(result->is_error()) << result->error_value();
    }
  }

  // TODO(fxbug.dev/124976): Unify rebind and production drivers.
  const char* GetRebindDriverSuffix() {
    return is_dfv2_ ? "libmsd_arm_rebind_dfv2.cm" : "libmsd_arm_rebind.cm";
  }
  const char* GetTestDriverSuffix() {
    return is_dfv2_ ? "libmsd_arm_test_dfv2.cm" : "libmsd_arm_test.cm";
  }

  const char* GetParentTopologicalPath() {
    // TODO(fxbug.dev/127515): Avoid hardcoding this path.
    return "/dev/sys/platform/05:06:17/aml-gpu-composite/aml-gpu/mali-composite";
  }

  bool is_dfv2() const { return is_dfv2_; }

 private:
  // True if the MSD uses the DFv2 APIs.
  bool is_dfv2_ = false;
  fidl::WireSyncClient<fuchsia_driver_registrar::DriverRegistrar> registrar_client_;
};

#endif  // SRC_GRAPHICS_DRIVERS_MSD_ARM_MALI_TESTS_INTEGRATION_DRIVER_REGISTRY_H_
