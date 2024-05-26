// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, AstroTest) {
  static const char* kDevicePaths[] = {
      "sys/platform/pt/astro",
      "sys/platform/pt/astro/post-init/post-init",
      "sys/platform/05:03:1/aml-gpio/gpio",
      "sys/platform/05:03:1/aml-gpio/gpio-init",
      "sys/platform/astro-buttons/astro-buttons/buttons",
      "sys/platform/05:00:2/i2c-0/aml-i2c",
      "sys/platform/05:00:2:1/i2c-1/aml-i2c",
      "sys/platform/05:00:2:2/i2c-2/aml-i2c",
      "sys/platform/aml_gpu/aml-gpu-composite/aml-gpu",
      "sys/platform/aml-usb-phy/aml_usb_phy",
      "sys/platform/bt-uart/bluetooth-composite-spec/aml-uart/bt-transport-uart",
      "sys/platform/bt-uart/bluetooth-composite-spec/aml-uart/bt-transport-uart/bt-hci-broadcom",

      // XHCI driver will not be loaded if we are in USB peripheral mode.
      // "xhci/xhci/usb-bus",

      "sys/platform/05:00:2:2/i2c-2/aml-i2c/i2c/i2c-2-44/backlight/ti-lp8556",
      "sys/platform/display/display/amlogic-display/display-coordinator",
      "sys/platform/canvas/aml-canvas",
      "sys/platform/tee/tee/optee",
      "sys/platform/05:00:f/raw_nand/aml-raw_nand/nand/bl2/skip-block",
      "sys/platform/05:00:f/raw_nand/aml-raw_nand/nand/tpl/skip-block",
      "sys/platform/05:00:f/raw_nand/aml-raw_nand/nand/fts/skip-block",
      "sys/platform/05:00:f/raw_nand/aml-raw_nand/nand/factory/skip-block",
      "sys/platform/05:00:f/raw_nand/aml-raw_nand/nand/zircon-b/skip-block",
      "sys/platform/05:00:f/raw_nand/aml-raw_nand/nand/zircon-a/skip-block",
      "sys/platform/05:00:f/raw_nand/aml-raw_nand/nand/zircon-r/skip-block",
      "sys/platform/05:00:f/raw_nand/aml-raw_nand/nand/sys-config/skip-block",
      "sys/platform/05:00:f/raw_nand/aml-raw_nand/nand/migration/skip-block",
      "sys/platform/05:00:7/aml_sdio/aml-sd-emmc/sdmmc",
      "sys/platform/05:00:7/aml_sdio/aml-sd-emmc/sdmmc/sdmmc-sdio",
      "sys/platform/05:00:7/aml_sdio/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1",
      "sys/platform/05:00:7/aml_sdio/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-2",

      "sys/platform/05:00:2/i2c-0/aml-i2c/i2c/i2c-0-57/tcs3400_light/tcs-3400",
      "sys/platform/astro-clk/clocks",
      "sys/platform/astro-clk/clocks/clock-init",
      "sys/platform/astro-i2s-audio-out/aml_tdm/astro-audio-i2s-out",
      "sys/platform/astro-audio-pdm-in/aml_pdm/astro-audio-pdm-in",
      "sys/platform/aml-secure-mem/aml_securemem/aml-securemem",
      "sys/platform/pwm/aml-pwm-device/pwm-4/pwm_init",

      // CPU Device.
      "sys/platform/aml-cpu",
      "class/cpu-ctrl/000",
      "sys/platform/aml-power-impl-composite/aml-power-impl-composite/power-impl/power-core/power-0/aml_cpu/s905d2-arm-a53",
      // LED.
      "sys/platform/gpio-light/aml_light",
      // RAM (DDR) control.
      "sys/platform/05:03:24/ram",

      // Power Device.
      "sys/platform/aml-power-impl-composite/aml-power-impl-composite",
      "sys/platform/aml-power-impl-composite/aml-power-impl-composite/power-impl/power-core",
      "sys/platform/aml-power-impl-composite/aml-power-impl-composite/power-impl/power-core/power-0",

      // Thermal
      "sys/platform/05:03:a/thermal",
      "sys/platform/05:03:28/thermal",
      "class/thermal/000",
      "class/thermal/001",

      // Thermistor/ADC
      "class/adc/000",
      "class/adc/001",
      "class/adc/002",
      "class/adc/003",
      "class/temperature/000",
      "class/temperature/001",
      "class/temperature/002",
      "class/temperature/003",

      // Registers Device.
      "sys/platform/registers",
#ifdef include_packaged_drivers
      "sys/platform/05:03:e/aml_video",

      // WLAN
      "sys/platform/05:00:7/aml_sdio/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1/wifi/brcmfmac-wlanphyimpl",
      "sys/platform/05:00:7/aml_sdio/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1/wifi/brcmfmac-wlanphyimpl/wlanphy",
#endif

  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));

  static const char* kTouchscreenDevicePaths[] = {
      "sys/platform/05:00:2:1/i2c-1/aml-i2c/i2c/i2c-1-56/focaltech_touch/focaltouch-HidDevice",
      "sys/platform/05:00:2:1/i2c-1/aml-i2c/i2c/i2c-1-93/gt92xx_touch/gt92xx-HidDevice",
  };
  ASSERT_NO_FATAL_FAILURE(device_enumeration::WaitForOne(
      cpp20::span(kTouchscreenDevicePaths, std::size(kTouchscreenDevicePaths))));
}

}  // namespace
