// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, CloverTest) {
  static const char* kDevicePaths[] = {
      "sys/platform/pt/clover",
      "sys/platform/05:08:1/aml-gpio",
      "sys/platform/05:00:35/aml-spinand/nand",
      "sys/platform/05:08:32/clocks",
      "sys/platform/00:00:1b/sysmem",
      "sys/platform/00:00:e/tee/optee",
      "sys/platform/05:08:a/thermal",
      "class/thermal/000",
      "sys/platform/05:08:24/ram",
      "sys/platform/05:00:2/i2c/aml-i2c",
      "sys/platform/05:00:19/spi-0/aml-spi-0/spi/spi-0-0",
      "sys/platform/00:00:29",                 // registers device
      "sys/platform/05:08:1d/aml-pwm-device",  // pwm
      "sys/platform/05:08:6/aml_sdio/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1",
      "sys/platform/05:08:31/dsp/aml-dsp",
      "sys/platform/05:08:b/aml-pl-mailbox0",
      "sys/platform/05:08:b:1/aml-pl-mailbox1",

      // USB
      "sys/platform/05:08:36/a1-usb-phy",
      // Enter into usb host mode
      "sys/platform/05:08:36/a1-usb-phy/xhci/xhci/xhci/usb-bus",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));
}

}  // namespace
