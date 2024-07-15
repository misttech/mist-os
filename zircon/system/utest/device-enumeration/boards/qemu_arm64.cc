// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, QemuArm64Test) {
  static const char* kNodeMonikers[] = {
      "dev.sys.platform.pt.qemu-bus",
      "dev.sys.platform.pl031.rtc",
      "dev.sys.platform.pci.00_00.0_",
  };

  VerifyNodes(kNodeMonikers);
}

}  // namespace
