// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, QemuX64Test) {
  static const char* kDevicePaths[] = {
      "sys/platform/00:00:1b/sysmem",

      "sys/platform/pt/acpi", "sys/platform/pt/acpi/acpi-pwrbtn",
      "sys/platform/pt/PCI0/bus/00:1f.2_/00:1f.2/ahci",
      // TODO(http://fxbug.dev/124283): Re-enable with new names after QEMU roll
      //"sys/platform/pt/acpi/acpi-_SB_/acpi-PCI0/acpi-ISA_/acpi-KBD_/pt/acpi-KBD_-composite/i8042/i8042-keyboard",
      //"sys/platform/pt/acpi/acpi-_SB_/acpi-PCI0/acpi-ISA_/acpi-KBD_/pt/acpi-KBD_-composite/i8042/i8042-mouse",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));
}

}  // namespace
