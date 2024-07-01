// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, NelsonTest) {
  static const char* kNodeMonikers[] = {
      "dev.sys.platform.pt.nelson",
      "dev.sys.platform.pt.nelson.post-init.post-init",
      "dev.sys.platform.gpio.aml-gpio.gpio",
      "dev.sys.platform.gpio.aml-gpio.gpio-init",
      "dev.sys.platform.gpio-h.aml-gpio.gpio",
      "dev.sys.platform.nelson-buttons.nelson-buttons.buttons",
      "dev.sys.platform.bt-uart.bluetooth-composite-spec.aml-uart.bt-transport-uart",
      "dev.sys.platform.bt-uart.bluetooth-composite-spec.aml-uart.bt-transport-uart.bt-hci-broadcom",
      "dev.sys.platform.i2c-0.i2c-0.aml-i2c",
      "dev.sys.platform.i2c-1.i2c-1.aml-i2c",
      "dev.sys.platform.i2c-2.i2c-2.aml-i2c",
      "dev.sys.platform.aml_gpu.aml-gpu-composite.aml-gpu",
      "dev.sys.platform.aml-usb-phy.aml_usb_phy.aml_usb_phy",
      "dev.sys.platform.nelson-audio-i2s-out.aml_tdm.nelson-audio-i2s-out",
      "dev.sys.platform.nelson-audio-pdm-in.aml_pdm.nelson-audio-pdm-in",
      "dev.sys.platform.registers",  // registers device

      // XHCI driver will not be loaded if we are in USB peripheral mode.
      // "xhci.xhci.usb-bus",

      "dev.sys.platform.i2c-2.i2c-2.aml-i2c.i2c.i2c-2-44.backlight.ti-lp8556",
      "dev.sys.platform.canvas.aml-canvas",
      "dev.sys.platform.tee.tee.optee",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.boot1.block",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.boot2.block",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.rpmb",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-000.block",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-001.block",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-002.block",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-003.block",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-004.block",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-005.block",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-006.block",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-007.block",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-008.block",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-009.block",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-010.block",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-011.block",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-012.block",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-013.block",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-014.block",
      "dev.sys.platform.nelson-emmc.nelson_emmc.aml-sd-emmc.sdmmc.sdmmc-mmc.user.block.part-015.block",
      "dev.sys.platform.i2c-0.i2c-0.aml-i2c.i2c.i2c-0-57.tcs3400_light.tcs-3400",
      "dev.sys.platform.aml-nna.aml_nna",
      "dev.sys.platform.nelson-clk.clocks",
      "dev.sys.platform.nelson-clk.clocks.clock-init",
      "dev.sys.platform.05_05_a.aml_thermal_pll.thermal",
      "dev.sys.platform.nelson-cpu",
      "dev.sys.platform.aml-secure-mem.aml_securemem.aml-securemem",
      "dev.sys.platform.pwm.aml-pwm-device.pwm-0",
      "dev.sys.platform.pwm.aml-pwm-device.pwm-1",
      "dev.sys.platform.pwm.aml-pwm-device.pwm-2",
      "dev.sys.platform.pwm.aml-pwm-device.pwm-3",
      "dev.sys.platform.pwm.aml-pwm-device.pwm-4",
      "dev.sys.platform.pwm.aml-pwm-device.pwm-5",
      "dev.sys.platform.pwm.aml-pwm-device.pwm-6",
      "dev.sys.platform.pwm.aml-pwm-device.pwm-7",
      "dev.sys.platform.pwm.aml-pwm-device.pwm-8",
      "dev.sys.platform.pwm.aml-pwm-device.pwm-9",
      "dev.sys.platform.aml-sdio.aml_sdio.aml-sd-emmc.sdmmc",
      "dev.sys.platform.aml-sdio.aml_sdio.aml-sd-emmc.sdmmc.sdmmc-sdio",
      "dev.sys.platform.aml-sdio.aml_sdio.aml-sd-emmc.sdmmc.sdmmc-sdio.sdmmc-sdio-1",
      "dev.sys.platform.aml-sdio.aml_sdio.aml-sd-emmc.sdmmc.sdmmc-sdio.sdmmc-sdio-2",

      "dev.sys.platform.display.display.amlogic-display.display-coordinator",
      "dev.sys.platform.i2c-2.i2c-2.aml-i2c.i2c.i2c-2-73.ti_ina231_mlb.ti-ina231",
      "dev.sys.platform.i2c-2.i2c-2.aml-i2c.i2c.i2c-2-64.ti_ina231_speakers.ti-ina231",
      "dev.sys.platform.i2c-0.i2c-0.aml-i2c.i2c.i2c-0-112.shtv3",
      "dev.sys.platform.gt6853-touch.gt6853_touch.gt6853",

      // Amber LED.
      "dev.sys.platform.gpio-light.aml_light",

      "dev.sys.platform.gpio-h.aml-gpio.gpio.gpio-82.spi_1.aml-spi-1.spi.spi-1-0.selina-composite.selina",

      "dev.sys.platform.aml-ram-ctl.ram",

      // Thermistor/ADC
      "dev.sys.platform.03_0a_27.thermistor.thermistor-device.therm-thread",
      "dev.sys.platform.03_0a_27.thermistor.thermistor-device.therm-audio",
      "dev.sys.platform.adc.aml-saradc.0",
      "dev.sys.platform.adc.aml-saradc.NELSON_THERMISTOR_THREAD",
      "dev.sys.platform.adc.aml-saradc.NELSON_THERMISTOR_AUDIO",
      "dev.sys.platform.adc.aml-saradc.3",

      "dev.sys.platform.i2c-2.i2c-2.aml-i2c.i2c.i2c-2-45.tas58xx.TAS5805m",
      "dev.sys.platform.i2c-2.i2c-2.aml-i2c.i2c.i2c-2-45.tas58xx.TAS5805m.brownout_protection",

      "dev.sys.platform.gpio-c.aml-gpio.gpio.gpio-50.spi_0.aml-spi-0.spi.spi-0-0",

#ifdef include_packaged_drivers
      // OpenThread
      "dev.sys.platform.gpio-c.aml-gpio.gpio.gpio-50.spi_0.aml-spi-0.spi.spi-0-0.nrf52811_radio.ot-radio",

      // WLAN
      "dev.sys.platform.aml-sdio.aml_sdio.aml-sd-emmc.sdmmc.sdmmc-sdio.sdmmc-sdio-1.wifi.brcmfmac-wlanphyimpl",
      "dev.sys.platform.aml-sdio.aml_sdio.aml-sd-emmc.sdmmc.sdmmc-sdio.sdmmc-sdio-1.wifi.brcmfmac-wlanphyimpl.wlanphy",
#endif

      "dev.sys.platform.i2c-2.i2c-2.aml-i2c.i2c.i2c-2-45.tas58xx.TAS5805m.brownout_protection.nelson-brownout-protection",

  };
  VerifyNodes(kNodeMonikers);

  static const char* kTouchscreenNodeMonikers[] = {
      // One of these touch devices could be on P0/P1 boards.
      "dev.sys.platform.nelson-buttons.nelson-buttons.buttons",
      // This is the only possible touch device for P2 and beyond.
      "dev.sys.platform.gt6853-touch.gt6853-touch.gt6853",
  };
  VerifyOneOf(kTouchscreenNodeMonikers);

  // TODO(b/324268831): Remove these once devfs is deprecated.
  static const char* kDevFsPaths[] = {
      "class/thermal/000", "class/adc/000",         "class/adc/001",         "class/adc/002",
      "class/adc/003",     "class/temperature/000", "class/temperature/001",
  };
  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevFsPaths, std::size(kDevFsPaths)));
  ASSERT_NO_FATAL_FAILURE(device_enumeration::WaitForClassDeviceCount("class/power-sensor", 2));
}

}  // namespace
