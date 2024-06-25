// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, SherlockTest) {
  static const char* kNodeMonikers[] = {
      "dev.sys.platform.pt.sherlock",
      "dev.sys.platform.pt.sherlock.post-init.post-init",
      "dev.sys.platform.gpio.aml-gpio.gpio",
      "dev.sys.platform.gpio.aml-gpio.gpio-init",
      "dev.sys.platform.sherlock-clk.clocks",
      "dev.sys.platform.sherlock-clk.clocks.clock-init",
      "dev.sys.platform.gpio-light.aml_light",
      "dev.sys.platform.i2c-0.i2c-0.aml-i2c",
      "dev.sys.platform.i2c-1.i2c-1.aml-i2c",
      "dev.sys.platform.i2c-2.i2c-2.aml-i2c",
      "dev.sys.platform.canvas.aml-canvas",
      "dev.sys.platform.05_04_a.aml_thermal_pll.thermal",
      "dev.sys.platform.display.display.amlogic-display.display-coordinator",
      "dev.sys.platform.aml-usb-phy.aml_usb_phy",

      // XHCI driver will not be loaded if we are in USB peripheral mode.
      // "xhci.xhci.usb-bus",

      "dev.sys.platform.sherlock-emmc.sherlock_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.boot1.block",
      "dev.sys.platform.sherlock-emmc.sherlock_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.boot2.block",
      "dev.sys.platform.sherlock-emmc.sherlock_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.rpmb",
      "dev.sys.platform.sherlock-emmc.sherlock_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-000.block",
      "dev.sys.platform.sherlock-emmc.sherlock_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-002.block",
      "dev.sys.platform.sherlock-emmc.sherlock_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-000.block",
      "dev.sys.platform.sherlock-emmc.sherlock_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-002.block",
      "dev.sys.platform.sherlock-emmc.sherlock_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-003.block",
      "dev.sys.platform.sherlock-emmc.sherlock_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-004.block",
      "dev.sys.platform.sherlock-emmc.sherlock_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-005.block",
      "dev.sys.platform.sherlock-emmc.sherlock_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-006.block",
      "dev.sys.platform.sherlock-emmc.sherlock_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-007.block",
      "dev.sys.platform.sherlock-emmc.sherlock_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-008.block",
      "dev.sys.platform.sherlock-emmc.sherlock_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-009.block",
      "dev.sys.platform.sherlock-emmc.sherlock_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-010.block",
      "dev.sys.platform.sherlock-sd-emmc.sherlock_sd_emmc.aml-sd-emmc.sdmmc.sdmmc-sdio.sdmmc-sdio-1",
      "dev.sys.platform.sherlock-sd-emmc.sherlock_sd_emmc.aml-sd-emmc.sdmmc.sdmmc-sdio.sdmmc-sdio-2",

      "dev.sys.platform.aml-nna.aml_nna",
      "dev.sys.platform.pwm",  // pwm
      "dev.sys.platform.gpio-light.aml_light",
      "dev.sys.platform.aml_gpu.aml-gpu-composite.aml-gpu",
      "dev.sys.platform.sherlock-pdm-audio-in.aml_pdm.sherlock-audio-pdm-in",
      "dev.sys.platform.sherlock-i2s-audio-out.aml_tdm.sherlock-audio-i2s-out",
      "dev.sys.platform.i2c-1.i2c-1.aml-i2c.i2c.i2c-1-56.focaltech_touch",
      "dev.sys.platform.tee.tee.optee",
      "dev.sys.platform.gpio-c.aml-gpio.gpio.gpio-50.spi_0.aml-spi-0.spi.spi-0-0",
      "dev.sys.platform.sherlock-buttons.sherlock-buttons.buttons",
      "dev.sys.platform.i2c-2.i2c-2.aml-i2c.i2c.i2c-2-44.backlight.ti-lp8556",
      "dev.sys.platform.i2c-0.i2c-0.aml-i2c.i2c.i2c-0-57.tcs3400_light.tcs-3400",
      "dev.sys.platform.aml-secure-mem.aml_securemem.aml-securemem",
      "dev.sys.platform.pwm.aml-pwm-device.pwm-4.pwm_init",
      "dev.sys.platform.aml-ram-ctl.ram",
      "dev.sys.platform.registers",  // registers device

      // CPU Devices.
      "dev.sys.platform.aml-cpu",
      "dev.sys.platform.05_04_a.aml_thermal_pll.thermal.aml_cpu_legacy.big-cluster",
      "dev.sys.platform.05_04_a.aml_thermal_pll.thermal.aml_cpu_legacy.little-cluster",

      // Thermal devices.
      "dev.sys.platform.05_04_a",
      "dev.sys.platform.aml-thermal-ddr",
      "dev.sys.platform.aml-thermal-ddr.thermal",

      "dev.sys.platform.adc",
      "dev.sys.platform.adc.aml-saradc.0",
      "dev.sys.platform.adc.aml-saradc.SHERLOCK_THERMISTOR_BASE",
      "dev.sys.platform.adc.aml-saradc.SHERLOCK_THERMISTOR_AUDIO",
      "dev.sys.platform.adc.aml-saradc.SHERLOCK_THERMISTOR_AMBIENT",

      // Audio
      "dev.sys.platform.i2c-0.i2c-0.aml-i2c.i2c.i2c-0-111.audio-tas5720-woofer",
      "dev.sys.platform.i2c-0.i2c-0.aml-i2c.i2c.i2c-0-108.audio-tas5720-left-tweeter",
      "dev.sys.platform.i2c-0.i2c-0.aml-i2c.i2c.i2c-0-109.audio-tas5720-right-tweeter",

      // LCD Bias
      "dev.sys.platform.i2c-2.i2c-2.aml-i2c.i2c.i2c-2-62",

      // Touchscreen
      "dev.sys.platform.i2c-1.i2c-1.aml-i2c.i2c.i2c-1-56.focaltech_touch.focaltouch-HidDevice",

#ifdef include_packaged_drivers

      "dev.sys.platform.mipi-csi2.aml-mipi",
      "dev.sys.platform.mipi-csi2.aml-mipi.imx227_sensor",
      "dev.sys.platform.mipi-csi2.aml-mipi.imx227_sensor.imx227.gdc",
      "dev.sys.platform.mipi-csi2.aml-mipi.imx227_sensor.imx227.ge2d",

      "dev.sys.platform.aml_video.aml_video",
      "dev.sys.platform.aml-video-enc.aml-video-enc",

      "dev.sys.platform.gpio-c.aml-gpio.gpio.gpio-50.spi_0.aml-spi-0.spi.spi-0-0.nrf52840_radio.ot-radio",

      // WLAN
      "dev.sys.platform.sherlock-sd-emmc.sherlock_sd_emmc.aml-sd-emmc.sdmmc.sdmmc-sdio.sdmmc-sdio-1.wifi.brcmfmac-wlanphyimpl",
      "dev.sys.platform.sherlock-sd-emmc.sherlock_sd_emmc.aml-sd-emmc.sdmmc.sdmmc-sdio.sdmmc-sdio-1.wifi.brcmfmac-wlanphyimpl.wlanphy",

      "dev.sys.platform.mipi-csi2.aml-mipi.imx227_sensor.imx227.isp",
      "dev.sys.platform.mipi-csi2.aml-mipi.imx227_sensor.imx227.isp.arm-isp.camera_controller",
#endif
  };
  VerifyNodes(kNodeMonikers);

  // TODO(b/324268831): Remove these once devfs is deprecated.
  static const char* kDevFsPaths[] = {
      "class/cpu-ctrl/000",    "class/cpu-ctrl/001",    "class/thermal/000",
      "class/thermal/001",     "class/adc/000",         "class/adc/001",
      "class/adc/002",         "class/adc/003",         "class/temperature/000",
      "class/temperature/001", "class/temperature/002",
  };
  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevFsPaths, std::size(kDevFsPaths)));
}

}  // namespace
