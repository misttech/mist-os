// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, Av400Test) {
  static const char* kDevicePaths[] = {
      "sys/platform/pt/av400",
      "sys/platform/05:07:1/aml-gpio",
      "sys/platform/05:07:1d",  // pwm
      "sys/platform/05:07:2c/clocks",
      "sys/platform/05:00:2/i2c/aml-i2c",
      "sys/platform/00:00:29",  // registers device
      "sys/platform/05:07:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc",
      "sys/platform/05:00:6/aml_sdio/aml-sd-emmc/sdmmc/sdmmc-sdio",
      "sys/platform/05:00:19/spi-1/aml-spi-1/spi/spi-1-0",
      "sys/platform/05:07:1d/aml-pwm-device/pwm-6/pwm-init",
      "sys/platform/05:07:9/ethernet_mac/aml-ethernet/dwmac/dwmac/eth_phy/phy_null_device",
      // TODO(https://fxbug.dev/117539): Update topopath when dwmac is off
      // netdevice migration.
      "sys/platform/05:07:9/ethernet_mac/aml-ethernet/dwmac/dwmac/Designware-MAC/netdevice-migration/network-device",
      "sys/platform/05:07:9/ethernet_mac/aml-ethernet",
      "sys/platform/05:07:2e/aml-rtc",
      "sys/platform/05:07:12:1/av400-i2s-audio-out",
      "sys/platform/05:07:12:2/av400-i2s-audio-in",
      "sys/platform/05:07:13/av400-pdm-audio-in",
      "sys/platform/05:07:b/aml-mailbox",
      "sys/platform/05:07:31/dsp/aml-dsp",

      // CPU Device
      "sys/platform/05:07:1e",
      "class/cpu-ctrl/000",
      "sys/platform/05:07:26/aml-power-impl-composite/power-impl/composite-pd-armcore/power-0/aml-cpu/a5-arm-a55",

      // USB
      "sys/platform/05:00:2f/aml-usb-crg-phy-v2",
      // Force to usb peripheral
      "sys/platform/05:00:2f/aml-usb-crg-phy-v2/aml-usb-crg-phy-v2/udc/udc/udc/usb-peripheral/function-000/cdc-eth-function",

      // Power Device.
      "sys/platform/05:07:26/aml-power-impl-composite/power-impl",
      "sys/platform/05:07:26/aml-power-impl-composite/power-impl/composite-pd-armcore",
      "sys/platform/05:07:26/aml-power-impl-composite/power-impl/composite-pd-armcore/power-0",

      // Thermal
      "sys/platform/05:07:a/thermal",
      "class/thermal/000",
      "sys/platform/00:00:1b/sysmem",
      "sys/platform/00:00:e/tee/optee",

      // RAM (DDR) control.
      "sys/platform/05:07:24/ram",

      "sys/platform/05:07:1/aml-gpio/gpio-35/av400-buttons/hid-buttons",
      "sys/platform/05:07:1c/aml-nna",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));
}

}  // namespace
