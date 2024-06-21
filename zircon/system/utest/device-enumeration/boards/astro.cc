// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, AstroTest) {
  static const char* kNodeMonikers[] = {
      "dev.sys.platform.pt.astro",
      "dev.sys.platform.pt.astro.post-init.post-init",
      "dev.sys.platform.gpio.aml-gpio.gpio",
      "dev.sys.platform.gpio.aml-gpio.gpio-init",
      "dev.sys.platform.astro-buttons.astro-buttons.buttons",
      "dev.sys.platform.i2c-0.i2c-0.aml-i2c",
      "dev.sys.platform.i2c-1.i2c-1.aml-i2c",
      "dev.sys.platform.i2c-2.i2c-2.aml-i2c",
      "dev.sys.platform.aml_gpu.aml-gpu-composite.aml-gpu",
      "dev.sys.platform.aml-usb-phy.aml_usb_phy",
      "dev.sys.platform.bt-uart.bluetooth-composite-spec.aml-uart.bt-transport-uart",
      "dev.sys.platform.bt-uart.bluetooth-composite-spec.aml-uart.bt-transport-uart.bt-hci-broadcom",

      // XHCI driver will not be loaded if we are in USB peripheral mode.
      // "xhci.xhci.usb-bus",

      "dev.sys.platform.i2c-2.i2c-2.aml-i2c.i2c.i2c-2-44.backlight.ti-lp8556",
      "dev.sys.platform.display.display.amlogic-display.display-coordinator",
      "dev.sys.platform.canvas.aml-canvas",
      "dev.sys.platform.tee.tee.optee",
      "dev.sys.platform.raw_nand.raw_nand.aml-raw_nand.nand.bl2.skip-block",
      "dev.sys.platform.raw_nand.raw_nand.aml-raw_nand.nand.tpl.skip-block",
      "dev.sys.platform.raw_nand.raw_nand.aml-raw_nand.nand.fts.skip-block",
      "dev.sys.platform.raw_nand.raw_nand.aml-raw_nand.nand.factory.skip-block",
      "dev.sys.platform.raw_nand.raw_nand.aml-raw_nand.nand.zircon-b.skip-block",
      "dev.sys.platform.raw_nand.raw_nand.aml-raw_nand.nand.zircon-a.skip-block",
      "dev.sys.platform.raw_nand.raw_nand.aml-raw_nand.nand.zircon-r.skip-block",
      "dev.sys.platform.raw_nand.raw_nand.aml-raw_nand.nand.sys-config.skip-block",
      "dev.sys.platform.raw_nand.raw_nand.aml-raw_nand.nand.migration.skip-block",
      "dev.sys.platform.raw_nand.raw_nand.aml-raw_nand.nand.fvm.ftl.block",
      "dev.sys.platform.aml-sdio.aml_sdio.aml-sd-emmc.sdmmc",
      "dev.sys.platform.aml-sdio.aml_sdio.aml-sd-emmc.sdmmc.sdmmc-sdio",
      "dev.sys.platform.aml-sdio.aml_sdio.aml-sd-emmc.sdmmc.sdmmc-sdio.sdmmc-sdio-1",
      "dev.sys.platform.aml-sdio.aml_sdio.aml-sd-emmc.sdmmc.sdmmc-sdio.sdmmc-sdio-2",

      "dev.sys.platform.i2c-0.i2c-0.aml-i2c.i2c.i2c-0-57.tcs3400_light.tcs-3400",
      "dev.sys.platform.astro-clk.clocks",
      "dev.sys.platform.astro-clk.clocks.clock-init",
      "dev.sys.platform.astro-i2s-audio-out.aml_tdm.astro-audio-i2s-out",
      "dev.sys.platform.astro-audio-pdm-in.aml_pdm.astro-audio-pdm-in",
      "dev.sys.platform.aml-secure-mem.aml_securemem.aml-securemem",
      "dev.sys.platform.pwm.aml-pwm-device.pwm-4.pwm_init",

      // CPU Device.
      "dev.sys.platform.aml-cpu",
      "dev.sys.platform.aml-power-impl-composite.aml-power-impl-composite.power-impl.power-core.power-0.aml_cpu.s905d2-arm-a53",
      // LED.
      "dev.sys.platform.gpio-light.aml_light",
      // RAM (DDR) control.
      "dev.sys.platform.aml-ram-ctl.ram",

      // Power Device.
      "dev.sys.platform.aml-power-impl-composite.aml-power-impl-composite",
      "dev.sys.platform.aml-power-impl-composite.aml-power-impl-composite.power-impl.power-core",
      "dev.sys.platform.aml-power-impl-composite.aml-power-impl-composite.power-impl.power-core.power-0",

      // Thermal
      "dev.sys.platform.aml-thermal-ddr.thermal",
      "dev.sys.platform.05_03_a.thermal",
      "dev.sys.platform.aml-thermal-ddr.thermal",

      // Thermistor.ADC
      "dev.sys.platform.adc.aml-saradc.ASTRO_THERMISTOR_SOC",
      "dev.sys.platform.adc.aml-saradc.ASTRO_THERMISTOR_WIFI",
      "dev.sys.platform.adc.aml-saradc.ASTRO_THERMISTOR_DSP",
      "dev.sys.platform.adc.aml-saradc.ASTRO_THERMISTOR_AMBIENT",
      "dev.sys.platform.03_03_27.thermistor.thermistor-device.therm-soc",
      "dev.sys.platform.03_03_27.thermistor.thermistor-device.therm-wifi",
      "dev.sys.platform.03_03_27.thermistor.thermistor-device.therm-dsp",
      "dev.sys.platform.03_03_27.thermistor.thermistor-device.therm-ambient",
      "dev.sys.platform.05_03_a.thermal",
      "dev.sys.platform.aml-thermal-ddr.thermal",

      // Registers Device.
      "dev.sys.platform.registers",
#ifdef include_packaged_drivers
      "dev.sys.platform.05:03:e.aml_video",

      // WLAN
      "dev.sys.platform.aml-sdio.aml_sdio.aml-sd-emmc.sdmmc.sdmmc-sdio.sdmmc-sdio-1.wifi.brcmfmac-wlanphyimpl",
      "dev.sys.platform.aml-sdio.aml_sdio.aml-sd-emmc.sdmmc.sdmmc-sdio.sdmmc-sdio-1.wifi.brcmfmac-wlanphyimpl.wlanphy",
#endif

  };
  VerifyNodes(kNodeMonikers);

  static const char* kTouchscreenNodeMonikers[] = {
      "dev.sys.platform.i2c-1.i2c-1.aml-i2c.i2c.i2c-1-56.focaltech_touch.focaltouch-HidDevice",
      "dev.sys.platform.i2c-1.i2c-1.aml-i2c.i2c.i2c-1-93.gt92xx_touch.gt92xx-HidDevice",
  };
  VerifyOneOf(kTouchscreenNodeMonikers);
}

}  // namespace
