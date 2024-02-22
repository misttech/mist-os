// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, AemuX64Test) {
  static const char* kDevicePaths[] = {
      "sys/platform/00:00:1b/sysmem",

      "sys/platform/pt/acpi",
      "sys/platform/pt/PCI0/bus/00:1f.2/00_1f_2/ahci",
      "sys/platform/pt/acpi/_SB_/PCI0/ISA_/KBD_/pt/KBD_-composite-spec/i8042/i8042-keyboard",
      "sys/platform/pt/acpi/_SB_/PCI0/ISA_/KBD_/pt/KBD_-composite-spec/i8042/i8042-mouse",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));

  static const char* kAemuDevicePaths[] = {
      "sys/platform/pt/PCI0/bus/00:01.0/00_01_0/virtio-input",
      "sys/platform/pt/PCI0/bus/00:02.0/00_02_0/virtio-input",
      "sys/platform/pt/PCI0/bus/00:0b.0/00_0b_0/goldfish-address-space",

      // Verify goldfish pipe root device created.
      "sys/platform/pt/acpi/_SB_/GFPP/pt/GFPP-composite-spec/goldfish-pipe",
      // Verify goldfish pipe child devices created.
      "sys/platform/pt/acpi/_SB_/GFPP/pt/GFPP-composite-spec/goldfish-pipe/goldfish-pipe-control",
      "sys/platform/pt/acpi/_SB_/GFPP/pt/GFPP-composite-spec/goldfish-pipe/goldfish-pipe-sensor",
      "sys/platform/pt/acpi/_SB_/GFSK/pt/GFSK-composite-spec/goldfish-sync",

      "sys/platform/pt/acpi/_SB_/GFPP/pt/GFPP-composite-spec/goldfish-pipe/goldfish-pipe-control/goldfish-control-2/goldfish-control",
      "sys/platform/pt/acpi/_SB_/GFPP/pt/GFPP-composite-spec/goldfish-pipe/goldfish-pipe-control/goldfish-control-2/goldfish-control/goldfish-display",
      "sys/platform/pt/acpi/_SB_/GFPP/pt/GFPP-composite-spec/goldfish-pipe/goldfish-pipe-control/goldfish-control-2",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kAemuDevicePaths, std::size(kAemuDevicePaths)));
}

}  // namespace
