// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, AtlasTest) {
  static const char* kDevicePaths[] = {
      "sys/platform/pt/pci/00:19.2_/pci-00:19.2-fidl/i2c-bus-9d64/i2c/i2c-3-26",
      "sys/platform/pt/pci/01:00.0_/pci-01:00.0-fidl/iwlwifi-wlanphyimpl",
      // Codec headphones.
      "sys/platform/pt/acpi/acpi-_SB_/acpi-PCI0/acpi-I2C4/acpi-MAXL/pt/acpi-MAXL-composite/MAX98373",
      "sys/platform/pt/acpi/acpi-_SB_/acpi-PCI0/acpi-I2C4/acpi-MAXR/pt/acpi-MAXR-composite/MAX98373",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));

  if (device_enumeration::IsDfv2Enabled()) {
    static const char* kDfv2DevicePaths[] = {
        "sys/platform/pt/acpi/acpi-_SB_/acpi-PCI0/acpi-I2C4/acpi-DLG7/pt/da7219-output",
        "sys/platform/pt/acpi/acpi-_SB_/acpi-PCI0/acpi-I2C4/acpi-DLG7/pt/da7219-input",
    };

    ASSERT_NO_FATAL_FAILURE(TestRunner(kDfv2DevicePaths, std::size(kDfv2DevicePaths)));
    return;
  }

  static const char* kDfv1DevicePaths[] = {
      "sys/platform/pt/acpi/acpi-_SB_/acpi-PCI0/acpi-I2C4/acpi-DLG7/pt/acpi-DLG7-composite/DA7219-output",
      "sys/platform/pt/acpi/acpi-_SB_/acpi-PCI0/acpi-I2C4/acpi-DLG7/pt/acpi-DLG7-composite/DA7219-input",
      "sys/platform/pt/pci/00:1f.3_/pci-00:1f.3-fidl/intel-hda-000/intel-sst-dsp",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDfv1DevicePaths, std::size(kDfv1DevicePaths)));

  // TODO(fxbug.dev/107847): Move this back to the normal kDevicePaths when wlanphy_dfv2 is
  // re-enabled.
  static const char* kDevicesThatFailInDfv2[] = {
      "sys/platform/pt/pci/01:00.0_/pci-01:00.0-fidl/iwlwifi-wlanphyimpl/wlanphy",
  };
  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicesThatFailInDfv2, std::size(kDevicesThatFailInDfv2)));
}

}  // namespace
