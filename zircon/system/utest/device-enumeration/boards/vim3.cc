// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, Vim3Test) {
  static const char* kDevicePaths[] = {
      "sys/platform/pt/vim3",
      "sys/platform/00:00:1b/sysmem",
      "sys/platform/00:00:1e/dw-dsi",

      "sys/platform/05:06:1/aml-gpio",
      "sys/platform/05:06:14/clocks",
      "sys/platform/05:00:2/aml-i2c",
      "sys/platform/05:00:2:2/aml-i2c",
      "sys/platform/05:00:2/aml-i2c/i2c/i2c-0-81/rtc",
      "sys/platform/05:00:2:2/aml-i2c/i2c/i2c-2-50",
      "sys/platform/05:06:9/ethernet_mac/aml-ethernet/dwmac/dwmac/eth_phy/phy_null_device",
      // TODO(https://fxbug.dev/117539): Update topopath when dwmac is off
      // netdevice migration.
      "sys/platform/05:06:9/ethernet_mac/aml-ethernet/dwmac/dwmac/Designware-MAC/netdevice-migration/network-device",
      "sys/platform/05:06:9/ethernet_mac/aml-ethernet",
      "sys/platform/05:00:7/aml_sd/aml-sd-emmc",
      "sys/platform/05:00:6/vim3-sdio/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1",
      "sys/platform/05:00:6/vim3-sdio/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-2",
      "sys/platform/05:00:6/vim3-sdio/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1/wifi/brcmfmac-wlanphy",
      "sys/platform/05:00:6/vim3-sdio/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1/wifi/brcmfmac-wlanphy/wlanphy",
      "sys/platform/05:06:1c/aml_nna",
      "sys/platform/00:00:29",  // registers device
      "sys/platform/05:06:17/aml_gpu/aml-gpu",
      "sys/platform/05:00:10/aml-canvas",
      "sys/platform/00:00:1e/dw-dsi/display/amlogic-display/display-coordinator",
      "sys/platform/05:06:2b/aml-hdmi",
      "sys/platform/05:06:1d",  // pwm
      "sys/platform/05:06:1d/aml-pwm-device/pwm-9/vreg/pwm-0-regulator",
      "sys/platform/05:06:1d/aml-pwm-device/pwm-9/vreg/pwm-9-regulator",
      "sys/platform/05:06:26/aml-power-impl-composite",
      "sys/platform/05:06:26/aml-power-impl-composite/power-impl/pd-big-core",
      "sys/platform/05:06:26/aml-power-impl-composite/power-impl/pd-little-core",
      "sys/platform/05:06:26",  // power

      // CPU devices.
      "sys/platform/05:06:1e",
      "sys/platform/05:06:26/aml-power-impl-composite/power-impl/pd-big-core/power-0/aml-cpu/a311d-arm-a73",
      "sys/platform/05:06:26/aml-power-impl-composite/power-impl/pd-big-core/power-0/aml-cpu/a311d-arm-a53",

      "sys/platform/05:00:2/aml-i2c/i2c/i2c-0-34/fusb302",

      // USB
      "sys/platform/05:03:2d/vim3_usb_phy",
      "sys/platform/05:03:2d/vim3_usb_phy/vim3_usb_phy/dwc2/dwc2/dwc2/usb-peripheral/function-000/cdc-eth-function",
      "sys/platform/05:03:2d/vim3_usb_phy/vim3_usb_phy/xhci/xhci",

      // USB 2.0 Hub
      "sys/platform/05:03:2d/vim3_usb_phy/vim3_usb_phy/xhci/xhci/xhci/usb-bus/000/usb-hub",

      // Thermal
      "sys/platform/05:06:28",
      "sys/platform/05:06:a",
      "class/thermal/000",

      // GPIO
      "sys/platform/05:00:2/aml-i2c/i2c/i2c-0-32/gpio-expander/ti-tca6408a/gpio-107",

      // Touch panel
      //
      // i2c device
      "sys/platform/05:00:2:2/aml-i2c/i2c/i2c-2-56",
      // interrupt pin
      "sys/platform/05:06:1/aml-gpio/gpio-21",
      // reset pin
      "sys/platform/05:00:2/aml-i2c/i2c/i2c-0-32/gpio-expander/ti-tca6408a/gpio-106",

      "sys/platform/05:00:2/aml-i2c/i2c/i2c-0-24/vim3-mcu",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));
}

}  // namespace
