// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.sysinfo/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, Vim3DeviceTreeTest) {
  static const char* kDevicePaths[] = {
      "sys/platform/pt",
      "sys/platform/dt-root",
      "sys/platform/interrupt-controller-ffc01000",
      "sys/platform/i2c-5000",
      "sys/platform/i2c-1c000",
      "sys/platform/clock-controller-ff63c000",
      "sys/platform/fuchsia-contiguous/sysmem",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));
}

}  // namespace
