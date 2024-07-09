// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, QemuX64Test) {
  const char* kNodeMonikers[] = {
      "dev.sys.platform.00_00_1b.sysmem",

      "dev.sys.platform.pt.acpi", "dev.sys.platform.pt.PCI0.bus.00_1f.2.00_1f_2.ahci",
      // TODO(https://fxbug.dev/42075162): Re-enable with new names after QEMU roll
      //"dev.sys.platform.pt.acpi._SB_.PCI0.ISA_.KBD_.pt.KBD_-composite-spec.i8042.i8042-keyboard",
      //"dev.sys.platform.pt.acpi._SB_.PCI0.ISA_.KBD_.pt.KBD_-composite-spec.i8042.i8042-mouse",
  };

  VerifyNodes(kNodeMonikers);
}

}  // namespace
