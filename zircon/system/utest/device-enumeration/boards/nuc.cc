// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, NucTest) {
  static const char* kDevicePaths[] = {
      "sys/platform/pt/PCI0/bus/00:02.0_/pci-00:02.0-fidl/intel_i915/intel-gpu-core",
      "sys/platform/pt/PCI0/bus/00:02.0_/pci-00:02.0-fidl/intel_i915/intel-display-controller/display-controller",
      "sys/platform/pt/PCI0/bus/00:14.0_/pci-00:14.0-fidl/xhci/usb-bus",
      "sys/platform/pt/PCI0/bus/00:15.0_/pci-00:15.0-fidl/i2c-bus-9d60",
      "sys/platform/pt/PCI0/bus/00:15.1_/pci-00:15.1-fidl/i2c-bus-9d61",
      "sys/platform/pt/PCI0/bus/00:17.0_/pci-00:17.0-fidl/ahci",
      // TODO(fxbug.dev/84037): Temporarily removed.
      // "pci-00:1f.3-fidl/intel-hda-000",
      // "pci-00:1f.3-fidl/intel-hda-controller",
      "sys/platform/pt/PCI0/bus/00:1f.6_/pci-00:1f.6-fidl/e1000",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));
}

}  // namespace
