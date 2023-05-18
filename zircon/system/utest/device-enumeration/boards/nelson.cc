// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, NelsonTest) {
  static const char* kDevicePaths[] = {
      "sys/platform/pt/nelson",
      "sys/platform/05:05:1/aml-gpio",
      "sys/platform/05:05:1:1/aml-gpio",
      "sys/platform/05:05:1/aml-gpio/gpio-5/nelson-buttons/hid-buttons",
      "sys/platform/05:00:3/bt-uart/aml-uart/bt-transport-uart",
      "sys/platform/05:00:3/bt-uart/aml-uart/bt-transport-uart/bt-hci-broadcom",
      "sys/platform/05:00:2/aml-i2c",
      "sys/platform/05:00:2:1/aml-i2c",
      "sys/platform/05:00:2:2/aml-i2c",
      "sys/platform/05:05:17/aml_gpu/aml-gpu",
      "sys/platform/05:0a:21/nelson-usb-phy",
      "sys/platform/05:05:12/aml_tdm/nelson-audio-i2s-out",
      "sys/platform/05:05:13/nelson-audio-pdm-in",
      "sys/platform/00:00:29",  // registers device

      // XHCI driver will not be loaded if we are in USB peripheral mode.
      // "xhci/xhci/usb-bus",

      "sys/platform/05:00:2:2/aml-i2c/i2c/i2c-2-44/backlight/ti-lp8556",
      "sys/platform/05:00:10/aml-canvas",
      "sys/platform/00:00:e/tee/optee",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/boot1/block",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/boot2/block",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/rpmb",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-000/block",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-001/block",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-002/block",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-003/block",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-004/block",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-005/block",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-006/block",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-007/block",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-008/block",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-009/block",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-010/block",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-011/block",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-012/block",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-013/block",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-014/block",
      "sys/platform/05:00:8/nelson-emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-015/block",
      "sys/platform/05:00:2/aml-i2c/i2c/i2c-0-57/tcs3400_light/tcs-3400",
      "sys/platform/05:05:1c/aml_nna",
      "sys/platform/05:05:22/clocks",
      "sys/platform/05:05:a/aml-thermal-pll/thermal",
      "class/thermal/000",
      // "sys/platform/05:03:1e/cpu",
      "sys/platform/05:03:1a/aml-secure-mem/aml-securemem",
      "sys/platform/05:05:1d/aml-pwm-device/pwm-0",
      "sys/platform/05:05:1d/aml-pwm-device/pwm-1",
      "sys/platform/05:05:1d/aml-pwm-device/pwm-2",
      "sys/platform/05:05:1d/aml-pwm-device/pwm-3",
      "sys/platform/05:05:1d/aml-pwm-device/pwm-4",
      "sys/platform/05:05:1d/aml-pwm-device/pwm-5",
      "sys/platform/05:05:1d/aml-pwm-device/pwm-6",
      "sys/platform/05:05:1d/aml-pwm-device/pwm-7",
      "sys/platform/05:05:1d/aml-pwm-device/pwm-8",
      "sys/platform/05:05:1d/aml-pwm-device/pwm-9",
      "sys/platform/05:00:6/aml-sdio/aml-sd-emmc/sdmmc",
      "sys/platform/05:00:6/aml-sdio/aml-sd-emmc/sdmmc/sdmmc-sdio",
      "sys/platform/05:00:6/aml-sdio/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1",
      "sys/platform/05:00:6/aml-sdio/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-2",
      "sys/platform/05:00:6/aml-sdio/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1/wifi/brcmfmac-wlanphy",
      "sys/platform/05:00:6/aml-sdio/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1/wifi/brcmfmac-wlanphy/wlanphy",
      "sys/platform/00:00:1e/dw-dsi",
      "sys/platform/00:00:1e/dw-dsi/display/amlogic-display/display-coordinator",
      "sys/platform/05:00:2:2/aml-i2c/i2c/i2c-2-73/ti-ina231-mlb/ti-ina231",
      "sys/platform/05:00:2:2/aml-i2c/i2c/i2c-2-64/ti-ina231-speakers/ti-ina231",
      "sys/platform/05:00:2/aml-i2c/i2c/i2c-0-112/shtv3",
      "sys/platform/1c:00:1/gt6853-touch/gt6853",

      // Amber LED.
      "sys/platform/05:00:1c/aml_light",

      "sys/platform/05:05:1:1/aml-gpio/gpio-82/spi-1/aml-spi-1/spi/spi-1-0/selina-composite/selina",

      "sys/platform/05:05:24/ram",

      "sys/platform/03:0a:27/thermistor-device/therm-thread",
      "sys/platform/03:0a:27/thermistor-device/therm-audio",

      "sys/platform/05:00:2:2/aml-i2c/i2c/i2c-2-45/tas58xx/TAS5805m",
      "sys/platform/05:00:2:2/aml-i2c/i2c/i2c-2-45/tas58xx/TAS5805m/brownout_protection",

      "sys/platform/05:05:1:2/aml-gpio/gpio-50/spi-0/aml-spi-0/spi/spi-0-0",
      "sys/platform/05:05:1:2/aml-gpio/gpio-50/spi-0/aml-spi-0/spi/spi-0-0/nrf52811-radio/ot-radio",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));

  static const char* kTouchscreenDevicePaths[] = {
      // One of these touch devices could be on P0/P1 boards.
      "sys/platform/05:05:1/aml-gpio/gpio-5/nelson-buttons/hid-buttons/hidbus_function/hid-device/InputReport",
      // This is the only possible touch device for P2 and beyond.
      "sys/platform/1c:00:1/gt6853-touch/gt6853",
  };
  ASSERT_NO_FATAL_FAILURE(device_enumeration::WaitForOne(
      cpp20::span(kTouchscreenDevicePaths, std::size(kTouchscreenDevicePaths))));
}

}  // namespace
