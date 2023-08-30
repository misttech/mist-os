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
  fidl::ClientEnd parent_device =
      magma::TestDeviceBase::GetParentDeviceFromId(MAGMA_VENDOR_ID_MALI);

  const char* kTestDriverPath = "libmsd_arm_test.cm";
  // The test driver will run unit tests on startup.
  magma::TestDeviceBase::RebindDevice(parent_device, kTestDriverPath);
  // Reload the production driver so later tests shouldn't be affected.
  auto cleanup = fit::defer([&]() {
    // TODO(fxbug.dev/124976): Unify rebind and production drivers.
    const char* kRebindDriverPath = "libmsd_arm_rebind.cm";
    magma::TestDeviceBase::RebindDevice(parent_device, kRebindDriverPath);
  });

  magma::TestDeviceBase test_base(MAGMA_VENDOR_ID_MALI);

  fidl::UnownedClientEnd<fuchsia_gpu_magma::TestDevice> channel{test_base.magma_channel()};
  const fidl::WireResult result = fidl::WireCall(channel)->GetUnitTestStatus();
  ASSERT_TRUE(result.ok()) << result.FormatDescription();
  const fidl::WireResponse response = result.value();
  ASSERT_EQ(response.status, ZX_OK) << zx_status_get_string(response.status);
}
