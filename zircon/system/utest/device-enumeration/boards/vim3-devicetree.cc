// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.sysinfo/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, Vim3DeviceTreeTest) {
  static const char* kDevicePaths[] = {
      "sys/platform/adc-9000",
      "sys/platform/adc-buttons/adc-buttons_group/adc-buttons",
      "sys/platform/arm-mali-0",
      "sys/platform/audio-controller-ff642000/audio-controller-ff642000_group/aml-g12-audio-composite",

      // bt-transport-uart is not included in bootfs on vim3.
      "sys/platform/bt-uart-ffd24000/bt-uart-ffd24000_group/aml-uart",
      // TODO(b/291154545): Add bluetooth paths when firmware is publicly available.
      // "sys/platform/bt-uart-ffd24000/bt-uart-ffd24000_group/aml-uart/bt-transport-uart/bt-hci-broadcom",

      "sys/platform/canvas-ff638000/aml-canvas",
      "sys/platform/clock-controller-ff63c000/clocks",
      "sys/platform/clock-controller-ff63c000/clocks/clock-init",
      "sys/platform/dsi-display-ff900000",
      "sys/platform/ethernet-phy-ff634000/ethernet-phy-ff634000_group/aml-ethernet/dwmac-ff3f0000_group/dwmac/eth_phy/phy_null_device",
      "sys/platform/fuchsia-sysmem/sysmem",
      "sys/platform/gpio-buttons/gpio-buttons_group/buttons",
      "sys/platform/gpio-controller-ff634400/aml-gpio/gpio-init",
      "sys/platform/gpio-controller-ff634400/aml-gpio/gpio",
      "sys/platform/gpio-controller-ff634400/aml-gpio/gpio/gpio-93/fusb302-22_group/fusb302",
      "sys/platform/hdmi-display-ff900000",
      "sys/platform/hrtimer-0/aml-hrtimer",
      "sys/platform/i2c-1c000",
      "sys/platform/i2c-1c000/i2c-1c000_group/aml-i2c/i2c/i2c-1-56/touchscreen-38_group/focaltouch-HidDevice",
      "sys/platform/i2c-5000",
      "sys/platform/i2c-5000/i2c-5000_group/aml-i2c/i2c/i2c-0-24",
      "sys/platform/i2c-5000/i2c-5000_group/aml-i2c/i2c/i2c-0-24/khadas-mcu-18_group/vim3-mcu",
      "sys/platform/i2c-5000/i2c-5000_group/aml-i2c/i2c/i2c-0-32/gpio-controller-20_group/ti-tca6408a/gpio",
      "sys/platform/interrupt-controller-ffc01000",
      "sys/platform/nna-ff100000/nna-ff100000_group/aml-nna",

      // SDIO
      "sys/platform/mmc-ffe03000/mmc-ffe03000_group/aml-sd-emmc/sdmmc",

      // SD card
      "sys/platform/mmc-ffe05000/mmc-ffe05000_group/aml-sd-emmc/sdmmc",

      // EMMC
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc/sdmmc-mmc/boot1/block",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc/sdmmc-mmc/boot2/block",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc/sdmmc-mmc/rpmb",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-000/block",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-001/block",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-002/block",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-003/block",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-004/block",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-005/block",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-006/block",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-007/block",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-008/block",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-009/block",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-010/block",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-011/block",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-012/block",
      "sys/platform/mmc-ffe07000/mmc-ffe07000_group/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-013/block",

      "sys/platform/phy-ffe09000/phy-ffe09000_group/aml_usb_phy",
      "sys/platform/phy-ffe09000/phy-ffe09000_group/aml_usb_phy/dwc2",
      "sys/platform/phy-ffe09000/phy-ffe09000_group/aml_usb_phy/dwc2/usb-ff400000_group/dwc2",
      "sys/platform/phy-ffe09000/phy-ffe09000_group/aml_usb_phy/dwc2/usb-ff400000_group/dwc2/usb-peripheral",
      "sys/platform/phy-ffe09000/phy-ffe09000_group/aml_usb_phy/xhci",
      "sys/platform/pt",
      "sys/platform/pt/dt-root",
      "sys/platform/pt/suspend",
      "sys/platform/pwm-ffd1b000/aml-pwm-device",
      "sys/platform/pwm-ffd1b000/aml-pwm-device/pwm-0/pwm_a-regulator_group/pwm_vreg_big",
      "sys/platform/pwm-ffd1b000/aml-pwm-device/pwm-4/pwm-init_group/aml-pwm-init",
      "sys/platform/pwm-ffd1b000/aml-pwm-device/pwm-9/pwm_a0_d-regulator_group/pwm_vreg_little",
      "sys/platform/register-controller-1000",
      "sys/platform/usb-ff500000/usb-ff500000_group/xhci/usb-bus",
      "sys/platform/video-decoder-ffd00000",

#ifdef include_packaged_drivers
      // RTC
      "sys/platform/i2c-5000/i2c-5000_group/aml-i2c/i2c/i2c-0-81/rtc",

      // GPU
      "sys/platform/gpu-ffe40000/gpu-ffe40000_group/aml-gpu",
#endif

  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));

  static const char* kDisplayDevicePaths[] = {
      "sys/platform/hdmi-display-ff900000/hdmi-display-ff900000_group/amlogic-display/display-coordinator",
      "sys/platform/dsi-display-ff900000/dsi-display-ff900000_group/amlogic-display/display-coordinator",
  };
  ASSERT_NO_FATAL_FAILURE(device_enumeration::WaitForOne(
      cpp20::span(kDisplayDevicePaths, std::size(kDisplayDevicePaths))));
}

}  // namespace
