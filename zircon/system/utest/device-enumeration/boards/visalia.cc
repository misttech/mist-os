// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, VisaliaTest) {
  static const char* kDevicePaths[] = {
      "sys/platform/14:01:1",
      "sys/platform/14:01:1/as370-gpio",
      "sys/platform/00:00:9",
      "sys/platform/00:00:9/dw-i2c",
      "sys/platform/14:01:2/as370-usb-phy",
      "sys/platform/14:01:a/as370-sdhci/sdhci/sdmmc/sdmmc-sdio/sdmmc-sdio-1",
      "sys/platform/14:01:a/as370-sdhci/sdhci/sdmmc/sdmmc-sdio/sdmmc-sdio-2",
      "sys/platform/14:01:2/as370-usb-phy/dwc2/dwc2-usb/dwc2",
      "sys/platform/00:00:22/cadence-hpnfc/nand/fvm/ftl/block",
      "sys/platform/00:00:22/cadence-hpnfc/nand/tzk_normal/skip-block",
      "sys/platform/00:00:22/cadence-hpnfc/nand/tzk_normalB/skip-block",
      "sys/platform/00:00:22/cadence-hpnfc/nand/bl_normal/skip-block",
      "sys/platform/00:00:22/cadence-hpnfc/nand/bl_normalB/skip-block",
      "sys/platform/00:00:22/cadence-hpnfc/nand/boot/skip-block",
      "sys/platform/00:00:22/cadence-hpnfc/nand/recovery/skip-block",
      "sys/platform/00:00:22/cadence-hpnfc/nand/fts/skip-block",
      "sys/platform/00:00:22/cadence-hpnfc/nand/factory_store/skip-block",
      "sys/platform/00:00:22/cadence-hpnfc/nand/key_1st/skip-block",
      "sys/platform/00:00:22/cadence-hpnfc/nand/key_2nd/skip-block",
      "sys/platform/00:00:22/cadence-hpnfc/nand/fastboot_1st/skip-block",
      "sys/platform/00:00:22/cadence-hpnfc/nand/fastboot_2nd/skip-block",
      "sys/platform/00:00:9/dw-i2c/i2c/i2c-0-102/power/as370-power/composite-pd-kBuckSoC",
      "sys/platform/14:00:8/thermal/as370-thermal",
      "sys/platform/10:02:5/lp5018-light/lp50xx-light",
      "sys/platform/00:00:9/dw-i2c/i2c/i2c-0-55/as370-touch/cy8cmbr3108/hid-device/InputReport",
      "sys/platform/00:00:9/dw-i2c/i2c/i2c-0-49/audio-max98373/MAX98373",
      "sys/platform/14:01:6/synaptics-dhub/as370-audio-in/as370-audio-in",
      "sys/platform/14:01:6/synaptics-dhub/as370-audio-out/as370-audio-out",
      "sys/platform/14:01:1/as370-gpio/gpio-11/as370-buttons/hid-buttons/hidbus_function/hid-device/InputReport",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));
  EXPECT_EQ(zx_system_get_num_cpus(), 4);
}

}  // namespace
