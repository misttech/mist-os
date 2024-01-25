// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.device/cpp/wire.h>
#include <fidl/fuchsia.gpu.magma/cpp/wire.h>
#include <lib/fdio/directory.h>
#include <lib/fit/defer.h>
#include <lib/magma_client/test_util/test_device_helper.h>
#include <lib/zx/channel.h>
#include <magma_intel_gen_defs.h>

#include <gtest/gtest.h>

// The test build of the MSD runs a bunch of unit tests automatically when it loads. We need to
// unload the normal MSD to replace it with the test MSD so we can run those tests and query the
// test results.
// TODO(https://fxbug.dev/42082221) - enable
#if ENABLE_HARDWARE_UNIT_TESTS
TEST(HardwareUnitTests, All) {
#else
TEST(HardwareUnitTests, DISABLED_All) {
#endif
  fidl::ClientEnd parent_device =
      magma::TestDeviceBase::GetParentDeviceFromId(MAGMA_VENDOR_ID_INTEL);

  const char* kTestDriverPath = "libmsd_intel_test.cm";
  // The test driver will run unit tests on startup.
  magma::TestDeviceBase::RebindDevice(parent_device, kTestDriverPath);

  // Reload the production driver so later tests shouldn't be affected.
  auto cleanup = fit::defer([&]() { magma::TestDeviceBase::RebindDevice(parent_device); });

  magma::TestDeviceBase test_base(MAGMA_VENDOR_ID_INTEL);

  fidl::UnownedClientEnd<fuchsia_gpu_magma::TestDevice> channel{test_base.magma_channel()};

  const fidl::WireResult result = fidl::WireCall(channel)->GetUnitTestStatus();

  EXPECT_EQ(ZX_OK, result.status()) << "Device connection lost, check syslog for any errors.";
  EXPECT_EQ(ZX_OK, result->status) << "Tests reported errors, check syslog.";
}
