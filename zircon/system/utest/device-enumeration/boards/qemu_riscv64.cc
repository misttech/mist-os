// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, QemuRiscv64Test) {
  // clang-format off
  static const char* kNodeMonikers[] = {
      "dev.sys.platform.goldfish-rtc",        // goldfish-rtc
      "dev.sys.platform.pt.PCI0.bus.00_00.0", // host bridge
      "dev.sys.platform.pt.qemu-riscv64",     // board driver
  };
  // clang-format on

  VerifyNodes(kNodeMonikers);
}

}  // namespace
