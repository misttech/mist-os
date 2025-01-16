// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.sysinfo/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, Vim3DeviceTreeTest) {
  static const char* kNodeMonikers[] = {
      "dev.sys.platform.adc-9000",
      "dev.sys.platform.adc-buttons.adc-buttons_group.adc-buttons",
      "dev.sys.platform.arm-mali-0",
      "dev.sys.platform.audio-controller-ff642000.audio-controller-ff642000_group.aml-g12-audio-composite",

      // bt-transport-uart is not included in bootfs on vim3.
      "dev.sys.platform.bt-uart-ffd24000.bt-uart-ffd24000_group.aml-uart",
      // TODO(b/291154545): Add bluetooth paths when firmware is publicly available.
      // "dev.sys.platform/bt-uart-ffd24000/bt-uart-ffd24000_group/aml-uart/bt-transport-uart/bt-hci-broadcom",

      "dev.sys.platform.canvas-ff638000.aml-canvas",
      "dev.sys.platform.clock-controller-ff63c000.clocks",
      "dev.sys.platform.clock-controller-ff63c000.clocks.clock-init",
      "dev.sys.platform.dsi-display-ff900000",
      "dev.sys.platform.ethernet-phy-ff634000.ethernet-phy-ff634000_group.aml-ethernet.dwmac-ff3f0000_group.dwmac.Designware-MAC.network-device",
      "dev.sys.platform.ethernet-phy-ff634000.ethernet-phy-ff634000_group.aml-ethernet.dwmac-ff3f0000_group.dwmac.eth_phy.phy_null_device",
      "dev.sys.platform.fuchsia-sysmem",
      "dev.sys.platform.gpio-buttons.gpio-buttons_group.buttons",
      "dev.sys.platform.gpio-controller-ff634400.aml-gpio.gpio-init",
      "dev.sys.platform.gpio-controller-ff634400.aml-gpio.gpio",
      "dev.sys.platform.gpio-controller-ff634400.aml-gpio.gpio.gpio-93.fusb302-22_group.fusb302",
      "dev.sys.platform.hdmi-display-ff900000",
      "dev.sys.platform.hrtimer-0.aml-hrtimer",
      "dev.sys.platform.i2c-5000",
      "dev.sys.platform.i2c-5000.i2c-5000_group.aml-i2c.i2c.i2c-0-24",
      "dev.sys.platform.i2c-5000.i2c-5000_group.aml-i2c.i2c.i2c-0-24.khadas-mcu-18_group.vim3-mcu",
      "dev.sys.platform.i2c-5000.i2c-5000_group.aml-i2c.i2c.i2c-0-32.gpio-controller-20_group.ti-tca6408a.gpio",
      "dev.sys.platform.interrupt-controller-ffc01000",
      "dev.sys.platform.nna-ff100000.nna-ff100000_group.aml-nna",

      // SDIO
      "dev.sys.platform.mmc-ffe03000.mmc-ffe03000_group.aml-sd-emmc.sdmmc",

      // SD card
      "dev.sys.platform.mmc-ffe05000.mmc-ffe05000_group.aml-sd-emmc.sdmmc",

      // EMMC
      "dev.sys.platform.mmc-ffe07000.mmc-ffe07000_group.aml-sd-emmc",
      "dev.sys.platform.mmc-ffe07000.mmc-ffe07000_group.aml-sd-emmc.sdmmc",
      "dev.sys.platform.mmc-ffe07000.mmc-ffe07000_group.aml-sd-emmc.sdmmc.sdmmc-mmc.boot1.block",
      "dev.sys.platform.mmc-ffe07000.mmc-ffe07000_group.aml-sd-emmc.sdmmc.sdmmc-mmc.boot2.block",
      "dev.sys.platform.mmc-ffe07000.mmc-ffe07000_group.aml-sd-emmc.sdmmc.sdmmc-mmc.rpmb",

      "dev.sys.platform.phy-ffe09000.phy-ffe09000_group.aml_usb_phy",
      "dev.sys.platform.phy-ffe09000.phy-ffe09000_group.aml_usb_phy.dwc2",
      "dev.sys.platform.phy-ffe09000.phy-ffe09000_group.aml_usb_phy.dwc2.usb-ff400000_group.dwc2",
      "dev.sys.platform.phy-ffe09000.phy-ffe09000_group.aml_usb_phy.dwc2.usb-ff400000_group.dwc2.usb-peripheral.function-000.cdc-eth-function",
      "dev.sys.platform.phy-ffe09000.phy-ffe09000_group.aml_usb_phy.xhci",
      "dev.sys.platform.power-controller.power-controller_group.power-impl.power-core.power-0",
      "dev.sys.platform.power-controller.power-controller_group.power-impl.power-core.power-0.cpu-controller-0_group.a311d-arm-a53",
      "dev.sys.platform.power-controller.power-controller_group.power-impl.power-core.power-0.cpu-controller-0_group.a311d-arm-a73",
      "dev.sys.platform.power-controller.power-controller_group.power-impl.power-core.power-1",
      "dev.sys.platform.pt",
      "dev.sys.platform.pt.dt-root",
      "dev.sys.platform.pt.suspend.generic-suspend-device",
      "dev.sys.platform.pwm-ffd1b000.aml-pwm-device",
      "dev.sys.platform.pwm-ffd1b000.aml-pwm-device.pwm-0.pwm_a-regulator_group.pwm_vreg_big",
      "dev.sys.platform.pwm-ffd1b000.aml-pwm-device.pwm-4.pwm-init_group.aml-pwm-init",
      "dev.sys.platform.pwm-ffd1b000.aml-pwm-device.pwm-9.pwm_a0_d-regulator_group.pwm_vreg_little",
      "dev.sys.platform.register-controller-1000",
      "dev.sys.platform.temperature-sensor-ff634800.temperature-sensor-ff634800_group.aml-trip-device",
      "dev.sys.platform.temperature-sensor-ff634c00.temperature-sensor-ff634c00_group.aml-trip-device",

      "dev.sys.platform.usb-ff500000.usb-ff500000_group.xhci",
      // USB 2.0 Hub
      // Ignored because we've had a spate of vim3 devices that seem to have
      // broken or flaky root hubs, and we don't make use of the XHCI bus in
      // any way so we'd rather ignore such failures than cause flakiness or
      // have to remove more devices from the fleet.
      // See b/296738636 for more information.
      // "dev.sys.platform/usb-ff500000/usb-ff500000_group/xhci/usb-bus",
      "dev.sys.platform.video-decoder-ffd00000",

#ifdef include_packaged_drivers
      // RTC
      "dev.sys.platform.i2c-5000.i2c-5000_group.aml-i2c.i2c.i2c-0-81.rtc",

      // WLAN
      "dev.sys.platform.mmc-ffe03000.mmc-ffe03000_group.aml-sd-emmc.sdmmc.sdmmc-sdio.sdmmc-sdio-1.wifi_group.brcmfmac-wlanphyimpl",
      "dev.sys.platform.mmc-ffe03000.mmc-ffe03000_group.aml-sd-emmc.sdmmc.sdmmc-sdio.sdmmc-sdio-1.wifi_group.brcmfmac-wlanphyimpl.wlanphy",

      // GPU
      "dev.sys.platform.gpu-ffe40000.gpu-ffe40000_group.aml-gpu",
#endif

  };

  VerifyNodes(kNodeMonikers);

  static const char* kDisplayNodeMonikers[] = {
      "dev.sys.platform.hdmi-display-ff900000.hdmi-display-ff900000_group.amlogic-display.display-coordinator",
      "dev.sys.platform.dsi-display-ff900000.dsi-display-ff900000_group.amlogic-display.display-coordinator",
  };
  VerifyOneOf(kDisplayNodeMonikers);
}

}  // namespace
