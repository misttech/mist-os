// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, NocturneTest) {
  static const char* kDevicePaths[] = {
      "sys/platform/pci/00:1f.3/intel-hda-000/input-stream-002",
      "sys/platform/pci/00:1f.3/intel-hda-000/output-stream-001",
      "sys/platform/pci/00:02.0/intel_i915/intel-gpu-core/msd-intel-gen",
      "sys/platform/pci/00:02.0/intel_i915/display-coordinator",
      "sys/platform/pt/acpi/TSR0",
      "sys/platform/pt/acpi/TSR1",
      "sys/platform/pt/acpi/TSR2",
      "sys/platform/pt/acpi/TSR3",
      "sys/platform/pt/acpi/acpi-lid/hid-device/InputReport",
      "sys/platform/pt/acpi/acpi-pwrbtn/hid-device/InputReport",
      "sys/platform/pci/00:15.0/i2c-bus-9d60/000a/i2c-hid/hid-device/InputReport",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));
}

}  // namespace
