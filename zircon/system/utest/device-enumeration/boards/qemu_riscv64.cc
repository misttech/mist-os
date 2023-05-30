// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, QemuRiscv64Test) {
  // clang-format off
  static const char* kDevicePaths[] = {
      "sys/platform/03:0d:b",             // goldfish-rtc
      "sys/platform/pt/PCI0/bus/00:00.0", // host bridge
      "sys/platform/pt/qemu-riscv64",     // board driver
  };
  // clang-format on

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));
}

}  // namespace
