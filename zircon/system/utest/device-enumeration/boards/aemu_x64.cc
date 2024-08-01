// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, AemuX64Test) {
  static const char* kNodeMonikers[] = {
      "dev.sys.platform.00_00_1b.sysmem",

      "dev.sys.platform.pt.acpi",
      "dev.sys.platform.pt.PCI0.bus.00_1f.2.00_1f_2.ahci",
      "dev.sys.platform.pt.acpi._SB_.PCI0.ISA_.KBD_.pt.KBD_-composite-spec.i8042.i8042-keyboard",
      "dev.sys.platform.pt.acpi._SB_.PCI0.ISA_.KBD_.pt.KBD_-composite-spec.i8042.i8042-mouse",
  };
  VerifyNodes(kNodeMonikers);

  static const char* kAemuNodeMonikers[] = {
      "dev.sys.platform.pt.PCI0.bus.00_0b.0.00_0b_0.goldfish-address-space",

      // Verify goldfish pipe root device created.
      "dev.sys.platform.pt.acpi._SB_.GFPP.pt.GFPP-composite-spec.goldfish-pipe",
      // Verify goldfish pipe child devices created.
      "dev.sys.platform.pt.acpi._SB_.GFPP.pt.GFPP-composite-spec.goldfish-pipe.goldfish-pipe-control",
      "dev.sys.platform.pt.acpi._SB_.GFPP.pt.GFPP-composite-spec.goldfish-pipe.goldfish-pipe-sensor",
      "dev.sys.platform.pt.acpi._SB_.GFSK.pt.GFSK-composite-spec.goldfish-sync",

      "dev.sys.platform.pt.acpi._SB_.GFPP.pt.GFPP-composite-spec.goldfish-pipe.goldfish-pipe-control.goldfish-control-2.goldfish-control",
      "dev.sys.platform.pt.acpi._SB_.GFPP.pt.GFPP-composite-spec.goldfish-pipe.goldfish-pipe-control.goldfish-control-2.goldfish-control.goldfish-display",
      "dev.sys.platform.pt.acpi._SB_.GFPP.pt.GFPP-composite-spec.goldfish-pipe.goldfish-pipe-control.goldfish-control-2",
  };

  VerifyNodes(kAemuNodeMonikers);
}

}  // namespace
