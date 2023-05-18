// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, SherlockTest) {
  static const char* kDevicePaths[] = {
      "sys/platform/pt/sherlock",
      "sys/platform/05:04:1/aml-gpio",
      "sys/platform/05:00:14/clocks",
      "sys/platform/05:00:1c/aml_light",
      "sys/platform/05:00:2/aml-i2c",
      "sys/platform/05:00:2:1/aml-i2c",
      "sys/platform/05:00:2:2/aml-i2c",
      "sys/platform/05:00:10/aml-canvas",
      "sys/platform/05:04:a/aml-thermal-pll/thermal",
      "sys/platform/00:00:1e/dw-dsi",
      "sys/platform/00:00:1e/dw-dsi/display/amlogic-display/display-coordinator",
      "sys/platform/05:00:18/aml-usb-phy-v2",

      // XHCI driver will not be loaded if we are in USB peripheral mode.
      // "xhci/xhci/usb-bus",

      "sys/platform/05:00:8/sherlock-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/boot1/block",
      "sys/platform/05:00:8/sherlock-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/boot2/block",
      "sys/platform/05:00:8/sherlock-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/rpmb",
      "sys/platform/05:00:8/sherlock-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-000/block",
      "sys/platform/05:00:8/sherlock-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-002/block",
      "sys/platform/05:00:8/sherlock-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-000/block",
      "sys/platform/05:00:8/sherlock-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-002/block",
      "sys/platform/05:00:8/sherlock-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-003/block",
      "sys/platform/05:00:8/sherlock-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-004/block",
      "sys/platform/05:00:8/sherlock-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-005/block",
      "sys/platform/05:00:8/sherlock-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-006/block",
      "sys/platform/05:00:8/sherlock-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-007/block",
      "sys/platform/05:00:8/sherlock-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-008/block",
      "sys/platform/05:00:8/sherlock-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-009/block",
      "sys/platform/05:00:8/sherlock-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-010/block",
      "sys/platform/05:00:6/sherlock-sd-emmc/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1",
      "sys/platform/05:00:6/sherlock-sd-emmc/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-2",
      "sys/platform/05:00:6/sherlock-sd-emmc/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1/wifi/brcmfmac-wlanphy",
      "sys/platform/05:00:6/sherlock-sd-emmc/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1/wifi/brcmfmac-wlanphy/wlanphy",
      "sys/platform/05:04:15/aml-mipi",
      "sys/platform/05:04:1c/aml_nna",
      "sys/platform/05:04:1d",  // pwm
      "sys/platform/05:04:15/aml-mipi/imx227_sensor/imx227/gdc",
      "sys/platform/05:04:15/aml-mipi/imx227_sensor/imx227/ge2d",
      "sys/platform/05:00:1c/aml_light",
      "sys/platform/05:04:15/aml-mipi/imx227_sensor",
      "sys/platform/05:04:15/aml-mipi/imx227_sensor/imx227/isp",
      "sys/platform/05:04:15/aml-mipi/imx227_sensor/imx227/isp/arm-isp/camera_controller",
      "sys/platform/05:04:e/aml-video",
      "sys/platform/05:04:23/aml-video-enc",
      "sys/platform/05:04:17/aml_gpu/aml-gpu",
      "sys/platform/05:04:13/sherlock-audio-pdm-in",
      "sys/platform/05:04:12:1/aml_tdm/sherlock-audio-i2s-out",
      "sys/platform/05:00:2:1/aml-i2c/i2c/i2c-1-56/focaltech_touch",
      "sys/platform/00:00:e/tee/optee",
      "sys/platform/05:04:1:1/aml-gpio/gpio-50/spi-0/aml-spi-0/spi/spi-0-0",
      "sys/platform/05:04:1/aml-gpio/gpio-4/sherlock-buttons/hid-buttons",
      "sys/platform/05:04:1:1/aml-gpio/gpio-50/spi-0/aml-spi-0/spi/spi-0-0/nrf52840-radio/ot-radio",
      "sys/platform/05:00:2:2/aml-i2c/i2c/i2c-2-44/backlight/ti-lp8556",
      "sys/platform/05:00:2/aml-i2c/i2c/i2c-0-57/tcs3400_light/tcs-3400",
      "sys/platform/05:04:1a/aml-secure-mem/aml-securemem",
      "sys/platform/05:04:1d/aml-pwm-device/pwm-4/pwm-init",
      "sys/platform/05:04:24/ram",
      "sys/platform/00:00:29",  // registers device

      // CPU Devices.
      "sys/platform/03:05:6",
      "class/cpu-ctrl/000",
      "class/cpu-ctrl/001",
      "sys/platform/05:04:a/aml-thermal-pll/thermal/aml_cpu_legacy/big-cluster",
      "sys/platform/05:04:a/aml-thermal-pll/thermal/aml_cpu_legacy/little-cluster",

      // Thermal devices.
      "sys/platform/05:04:a",
      "sys/platform/05:04:28",
      "class/thermal/000",
      "class/thermal/001",

      "class/adc/000",
      "class/adc/001",
      "class/adc/002",
      "class/adc/003",
      "class/temperature/000",
      "class/temperature/001",
      "class/temperature/002",

      // LCD Bias
      "sys/platform/05:00:2:2/aml-i2c/i2c/i2c-2-62",

      // Touchscreen
      "sys/platform/05:00:2:1/aml-i2c/i2c/i2c-1-56/focaltech_touch/focaltouch HidDevice/hid-device/InputReport",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));
}

}  // namespace
