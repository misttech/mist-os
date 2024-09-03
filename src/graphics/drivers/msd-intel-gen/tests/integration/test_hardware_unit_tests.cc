// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

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
  // TODO(https://fxbug.dev/328808539)
}
