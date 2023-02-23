// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, QemuArm64Test) {
  static const char* kDevicePaths[] = {
      "sys/platform/pt/qemu-bus",
      "sys/platform/00:00:6/rtc",
      "sys/platform/pci/00:00.0",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));
}

}  // namespace
