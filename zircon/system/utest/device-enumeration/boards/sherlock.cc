// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, SherlockTest) {
  static const char* kDevicePaths[] = {
      "sys/platform/pt/sherlock",
      "sys/platform/pt/sherlock/post-init/post-init",
      "sys/platform/05:04:1/aml-gpio/gpio",
      "sys/platform/05:04:1/aml-gpio/gpio-init",
      "sys/platform/sherlock-clk/clocks",
      "sys/platform/sherlock-clk/clocks/clock-init",
      "sys/platform/gpio-light/aml_light",
      "sys/platform/i2c-0/i2c-0/aml-i2c",
      "sys/platform/i2c-1/i2c-1/aml-i2c",
      "sys/platform/i2c-2/i2c-2/aml-i2c",
      "sys/platform/canvas/aml-canvas",
      "sys/platform/05:04:a/aml_thermal_pll/thermal",
      "sys/platform/display/display/amlogic-display/display-coordinator",
      "sys/platform/aml-usb-phy/aml_usb_phy",

      // XHCI driver will not be loaded if we are in USB peripheral mode.
      // "xhci/xhci/usb-bus",

      "sys/platform/sherlock-emmc/sherlock_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/boot1/block",
      "sys/platform/sherlock-emmc/sherlock_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/boot2/block",
      "sys/platform/sherlock-emmc/sherlock_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/rpmb",
      "sys/platform/sherlock-emmc/sherlock_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-000/block",
      "sys/platform/sherlock-emmc/sherlock_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-002/block",
      "sys/platform/sherlock-emmc/sherlock_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-000/block",
      "sys/platform/sherlock-emmc/sherlock_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-002/block",
      "sys/platform/sherlock-emmc/sherlock_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-003/block",
      "sys/platform/sherlock-emmc/sherlock_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-004/block",
      "sys/platform/sherlock-emmc/sherlock_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-005/block",
      "sys/platform/sherlock-emmc/sherlock_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-006/block",
      "sys/platform/sherlock-emmc/sherlock_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-007/block",
      "sys/platform/sherlock-emmc/sherlock_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-008/block",
      "sys/platform/sherlock-emmc/sherlock_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-009/block",
      "sys/platform/sherlock-emmc/sherlock_emmc/aml-sd-emmc/sdmmc/sdmmc-mmc/user/block/part-010/block",
      "sys/platform/sherlock-sd-emmc/sherlock_sd_emmc/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1",
      "sys/platform/sherlock-sd-emmc/sherlock_sd_emmc/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-2",

      "sys/platform/aml-nna/aml_nna",
      "sys/platform/pwm",  // pwm
      "sys/platform/gpio-light/aml_light",
      "sys/platform/aml_gpu/aml-gpu-composite/aml-gpu",
      "sys/platform/sherlock-pdm-audio-in/aml_pdm/sherlock-audio-pdm-in",
      "sys/platform/sherlock-i2s-audio-out/aml_tdm/sherlock-audio-i2s-out",
      "sys/platform/i2c-1/i2c-1/aml-i2c/i2c/i2c-1-56/focaltech_touch",
      "sys/platform/tee/tee/optee",
      "sys/platform/gpio-c/aml-gpio/gpio/gpio-50/spi_0/aml-spi-0/spi/spi-0-0",
      "sys/platform/sherlock-buttons/sherlock-buttons/buttons",
      "sys/platform/i2c-2/i2c-2/aml-i2c/i2c/i2c-2-44/backlight/ti-lp8556",
      "sys/platform/i2c-0/i2c-0/aml-i2c/i2c/i2c-0-57/tcs3400_light/tcs-3400",
      "sys/platform/aml-secure-mem/aml_securemem/aml-securemem",
      "sys/platform/pwm/aml-pwm-device/pwm-4/pwm_init",
      "sys/platform/aml-ram-ctl/ram",
      "sys/platform/registers",  // registers device

      // CPU Devices.
      "sys/platform/aml-cpu",
      "class/cpu-ctrl/000",
      "class/cpu-ctrl/001",
      "sys/platform/05:04:a/aml_thermal_pll/thermal/aml_cpu_legacy/big-cluster",
      "sys/platform/05:04:a/aml_thermal_pll/thermal/aml_cpu_legacy/little-cluster",

      // Thermal devices.
      "sys/platform/05:04:a",
      "sys/platform/aml-thermal-ddr",
      "class/thermal/000",
      "class/thermal/001",

      "class/adc/000",
      "class/adc/001",
      "class/adc/002",
      "class/adc/003",
      "class/temperature/000",
      "class/temperature/001",
      "class/temperature/002",

      // Audio
      // TODO(b/324598947): Re-enable once we figure out how to get them to stop flaking
      // in device-enumeration-test. The device nodes show up, but the device-enumeration-test
      // sometimes fail to wait for them.
      // "sys/platform/i2c-0/i2c-0/aml-i2c/i2c/i2c-0-111/audio-tas5720-woofer",
      // "sys/platform/i2c-0/i2c-0/aml-i2c/i2c/i2c-0-108/audio-tas5720-left-tweeter",
      // "sys/platform/i2c-0/i2c-0/aml-i2c/i2c/i2c-0-109/audio-tas5720-right-tweeter",

      // LCD Bias
      "sys/platform/i2c-2/i2c-2/aml-i2c/i2c/i2c-2-62",

      // Touchscreen
      "sys/platform/i2c-1/i2c-1/aml-i2c/i2c/i2c-1-56/focaltech_touch/focaltouch-HidDevice",

#ifdef include_packaged_drivers

      "sys/platform/mipi-csi2/aml-mipi",
      "sys/platform/mipi-csi2/aml-mipi/imx227_sensor",
      "sys/platform/mipi-csi2/aml-mipi/imx227_sensor/imx227/gdc",
      "sys/platform/mipi-csi2/aml-mipi/imx227_sensor/imx227/ge2d",

      "sys/platform/aml_video/aml_video",
      "sys/platform/aml-video-enc/aml-video-enc",

      "sys/platform/gpio-c/aml-gpio/gpio/gpio-50/spi_0/aml-spi-0/spi/spi-0-0/nrf52840_radio/ot-radio",

      // WLAN
      "sys/platform/sherlock-sd-emmc/sherlock_sd_emmc/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1/wifi/brcmfmac-wlanphyimpl",
      "sys/platform/sherlock-sd-emmc/sherlock_sd_emmc/aml-sd-emmc/sdmmc/sdmmc-sdio/sdmmc-sdio-1/wifi/brcmfmac-wlanphyimpl/wlanphy",

      "sys/platform/mipi-csi2/aml-mipi/imx227_sensor/imx227/isp",
      "sys/platform/mipi-csi2/aml-mipi/imx227_sensor/imx227/isp/arm-isp/camera_controller",
#endif
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));
}

}  // namespace
