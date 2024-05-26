// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, Vim3Test) {
  static const char* kDevicePaths[] = {
      "sys/platform/pt/vim3",
      "sys/platform/00:00:1b/sysmem",

      "sys/platform/05:06:1/aml-gpio/gpio",
      "sys/platform/05:06:1/aml-gpio/gpio-init",
      "sys/platform/vim3-clk/clocks",
      "sys/platform/vim3-clk/clocks/clock-init",
      "sys/platform/05:00:2/i2c-0/aml-i2c",
      "sys/platform/05:00:2:2/i2c-2/aml-i2c",
      "sys/platform/05:00:2:2/i2c-2/aml-i2c/i2c/i2c-2-50",
      "sys/platform/audio-composite/audio-composite-composite-spec/aml-g12-audio-composite",
      "sys/platform/ethernet_mac/ethernet_mac/aml-ethernet/dwmac/dwmac/eth_phy/phy_null_device",
      "sys/platform/buttons/function-button/adc-buttons",
      "sys/platform/vim3-buttons/vim3-buttons/buttons",
      "sys/platform/hrtimer/aml-hrtimer",

      // bt-transport-uart is not included in bootfs on vim3.
      "sys/platform/bt-uart/bluetooth-composite-spec/aml-uart",
      // TODO(b/291154545): Add bluetooth paths when firmware is publicly available.
      // "sys/platform/bt-uart/bluetooth-composite-spec/aml-uart/bt-transport-uart/bt-hci-broadcom",

      // TODO(https://fxbug.dev/42068759): Update topopath when dwmac is off
      // netdevice migration.
      "sys/platform/ethernet_mac/ethernet_mac/aml-ethernet/dwmac/dwmac/Designware-MAC/netdevice-migration/network-device",
      "sys/platform/ethernet_mac/ethernet_mac/aml-ethernet",
      "sys/platform/aml_sd/aml_sd/aml-sd-emmc/sdmmc",
      "sys/platform/05:00:6/vim3_sdio/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1",
      "sys/platform/05:00:6/vim3_sdio/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-2",
      "sys/platform/aml-nna/aml_nna",
      "sys/platform/registers",  // registers device
      "sys/platform/canvas/aml-canvas",
      "sys/platform/pwm",  // pwm
      "sys/platform/pwm/aml-pwm-device/pwm-4/pwm_init",
      "sys/platform/pwm/aml-pwm-device/pwm-0/pwm_vreg_big/pwm_vreg_big",
      "sys/platform/pwm/aml-pwm-device/pwm-9/pwm_vreg_little/pwm_vreg_little",
      "sys/platform/aml-power-impl-composite/aml-power-impl-composite",
      "sys/platform/aml-power-impl-composite/aml-power-impl-composite/power-impl/power-core/power-0",
      "sys/platform/aml-power-impl-composite/aml-power-impl-composite/power-impl/power-core/power-1",
      "sys/platform/aml-power-impl-composite",  // power

      // EMMC
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc",
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/boot1/block",
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/boot2/block",
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/rpmb",
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-000/block",
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-001/block",
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-002/block",
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-003/block",
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-004/block",
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-005/block",
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-006/block",
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-007/block",
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-008/block",
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-009/block",
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-010/block",
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-011/block",
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-012/block",
      "sys/platform/05:00:8/aml_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-013/block",

      // CPU devices.
      "sys/platform/aml-cpu",
      "sys/platform/aml-power-impl-composite/aml-power-impl-composite/power-impl/power-core/power-0/aml_cpu/a311d-arm-a73",
      "sys/platform/aml-power-impl-composite/aml-power-impl-composite/power-impl/power-core/power-0/aml_cpu/a311d-arm-a53",

      "sys/platform/05:06:1/aml-gpio/gpio/gpio-93/fusb302",

      // USB
      "sys/platform/usb-phy-pdev/usb-phy-composite",
      "sys/platform/usb-phy-pdev/usb-phy-composite/aml_usb_phy/dwc2/dwc2-composite/dwc2/usb-peripheral/function-000/cdc-eth-function",
      "sys/platform/usb-phy-pdev/usb-phy-composite/aml_usb_phy/xhci",
      "sys/platform/xhci-pdev/xhci-composite/xhci",

      // USB 2.0 Hub
      // Ignored because we've had a spate of vim3 devices that seem to have
      // broken or flaky root hubs, and we don't make use of the XHCI bus in
      // any way so we'd rather ignore such failures than cause flakiness or
      // have to remove more devices from the fleet.
      // See b/296738636 for more information.
      // "sys/platform/xhci-pdev/xhci-composite/xhci/usb-bus/000/usb-hub",

      // Temperature Sensors / Trip Point Devices.
      "sys/platform/05:06:39/pll-temp-sensor/aml-trip-device",         // PLL Temperature Sensor
      "sys/platform/ddr-temp-sensor/ddr-temp-sensor/aml-trip-device",  // DDR Temperature Sensor

      // GPIO
      "sys/platform/05:00:2/i2c-0/aml-i2c/i2c/i2c-0-32/gpio-expander/ti-tca6408a/gpio/gpio-7",

      // Touch panel
      //
      // i2c device
      "sys/platform/05:00:2:2/i2c-2/aml-i2c/i2c/i2c-2-56",
      // interrupt pin
      "sys/platform/05:06:1/aml-gpio/gpio/gpio-21",
      // reset pin
      "sys/platform/05:00:2/i2c-0/aml-i2c/i2c/i2c-0-32/gpio-expander/ti-tca6408a/gpio/gpio-6",

      "sys/platform/05:00:2/i2c-0/aml-i2c/i2c/i2c-0-24/mcu-composite/vim3-mcu",

      // Suspend HAL
      "sys/platform/vim3-suspend/aml-suspend-device",

      // ADC
      "sys/platform/adc/aml-saradc",

      // Button
      "sys/platform/buttons/function-button/adc-buttons",

#ifdef include_packaged_drivers

      // RTC
      "sys/platform/05:00:2/i2c-0/aml-i2c/i2c/i2c-0-81/rtc",

      // WLAN
      "sys/platform/05:00:6/vim3_sdio/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1/wifi/brcmfmac-wlanphyimpl",
      "sys/platform/05:00:6/vim3_sdio/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1/wifi/brcmfmac-wlanphyimpl/wlanphy",

      // GPU
      "sys/platform/aml_gpu/aml-gpu-composite/aml-gpu",

#endif

  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));

  static const char* kDisplayDevicePaths[] = {
      "sys/platform/hdmi-display/hdmi-display/amlogic-display/display-coordinator",
      "sys/platform/hdmi-display/dsi-display/amlogic-display/display-coordinator",
  };
  ASSERT_NO_FATAL_FAILURE(device_enumeration::WaitForOne(
      cpp20::span(kDisplayDevicePaths, std::size(kDisplayDevicePaths))));
}

}  // namespace
