// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.gpu.magma/cpp/wire.h>
#include <lib/fit/defer.h>
#include <lib/magma/magma.h>
#include <lib/zx/channel.h>

#include <shared_mutex>
#include <thread>

#include <gtest/gtest.h>

#include "driver_registry.h"
#include "helper/test_device_helper.h"
#include "magma_util/short_macros.h"
#include "magma_vendor_queries.h"

// The test build of the MSD runs a bunch of unit tests automatically when it loads. We need to
// unload the normal MSD to replace it with the test MSD so we can run those tests and query the
// test results.
TEST(UnitTests, UnitTests) {
  RegisteredTestDriver test_driver;
  ASSERT_NO_FATAL_FAILURE(test_driver.Init());
  std::optional<fidl::ClientEnd<fuchsia_device::Controller>> parent_device;
  if (test_driver.is_dfv2()) {
    auto parent_device_result = component::Connect<fuchsia_device::Controller>(
        std::string(test_driver.GetParentTopologicalPath()) + "/device_controller");

    EXPECT_EQ(ZX_OK, parent_device_result.status_value());
    parent_device = std::move(*parent_device_result);
  } else {
    parent_device = magma::TestDeviceBase::GetParentDeviceFromId(MAGMA_VENDOR_ID_MALI);
  }
  // The test driver will run unit tests on startup.
  magma::TestDeviceBase::RebindDevice(*parent_device, test_driver.GetTestDriverSuffix());
  // Reload the production driver so later tests shouldn't be affected.
  auto cleanup = fit::defer([&]() {
    magma::TestDeviceBase::RebindDevice(*parent_device, test_driver.GetRebindDriverSuffix());
  });

  magma::TestDeviceBase test_base(MAGMA_VENDOR_ID_MALI);

  fidl::UnownedClientEnd<fuchsia_gpu_magma::TestDevice> channel{test_base.magma_channel()};
  const fidl::WireResult result = fidl::WireCall(channel)->GetUnitTestStatus();
  ASSERT_TRUE(result.ok()) << result.FormatDescription();
  const fidl::WireResponse response = result.value();
  ASSERT_EQ(response.status, ZX_OK) << zx_status_get_string(response.status);
}
