// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, AemuX64Test) {
  static const char* kDevicePaths[] = {
      "sys/platform/00:00:1b/sysmem",

      "sys/platform/pt/acpi",
      "sys/platform/pt/acpi/acpi-pwrbtn",
      "sys/platform/pt/PCI0/bus/00:1f.2_/00:1f.2/ahci",
      "sys/platform/pt/acpi/acpi-_SB_/acpi-PCI0/acpi-ISA_/acpi-KBD_/pt/acpi-KBD_-composite/i8042/i8042-keyboard",
      "sys/platform/pt/acpi/acpi-_SB_/acpi-PCI0/acpi-ISA_/acpi-KBD_/pt/acpi-KBD_-composite/i8042/i8042-mouse",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));

  static const char* kAemuDevicePaths[] = {
      "sys/platform/pt/PCI0/bus/00:01.0_/00:01.0/virtio-input",
      "sys/platform/pt/PCI0/bus/00:02.0_/00:02.0/virtio-input",
      "sys/platform/pt/PCI0/bus/00:0b.0_/00:0b.0/goldfish-address-space",

      // Verify goldfish pipe root device created.
      "sys/platform/pt/acpi/acpi-_SB_/acpi-GFPP/pt/acpi-GFPP-composite/goldfish-pipe",
      // Verify goldfish pipe child devices created.
      "sys/platform/pt/acpi/acpi-_SB_/acpi-GFPP/pt/acpi-GFPP-composite/goldfish-pipe/goldfish-pipe-control",
      "sys/platform/pt/acpi/acpi-_SB_/acpi-GFPP/pt/acpi-GFPP-composite/goldfish-pipe/goldfish-pipe-sensor",
      "sys/platform/pt/acpi/acpi-_SB_/acpi-GFSK/pt/acpi-GFSK-composite/goldfish-sync",

      "sys/platform/pt/acpi/acpi-_SB_/acpi-GFPP/pt/acpi-GFPP-composite/goldfish-pipe/goldfish-pipe-control/goldfish-control-2/goldfish-control",
      "sys/platform/pt/acpi/acpi-_SB_/acpi-GFPP/pt/acpi-GFPP-composite/goldfish-pipe/goldfish-pipe-control/goldfish-control-2/goldfish-control/goldfish-display",
      "sys/platform/pt/acpi/acpi-_SB_/acpi-GFPP/pt/acpi-GFPP-composite/goldfish-pipe/goldfish-pipe-control/goldfish-control-2",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kAemuDevicePaths, std::size(kAemuDevicePaths)));
}

}  // namespace
