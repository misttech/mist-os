// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.sysinfo/cpp/wire.h>
#include <lib/component/incoming/cpp/protocol.h>

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, Vim3DeviceTreeTest) {
  static const char* kDevicePaths[] = {
      "sys/platform/pt",
      "sys/platform/dt-root",
      "sys/platform/interrupt-controller-ffc01000",
      "sys/platform/i2c-5000",
      "sys/platform/i2c-1c000",
      "sys/platform/clock-controller-ff63c000/clocks",
      "sys/platform/clock-controller-ff63c000/clocks/clock-init",
      "sys/platform/fuchsia-contiguous/sysmem",
      "sys/platform/register-controller-1000/registers-device",
      "sys/platform/nna-ff100000/nna-ff100000_group/aml-nna",
      "sys/platform/canvas-ff638000/aml-canvas",
      "sys/platform/adc-9000",
      "sys/platform/gpio-controller-ff634400/aml-gpio/gpio",
      "sys/platform/gpio-controller-ff634400/aml-gpio/gpio-init",
      "sys/platform/gpu-ffe40000/gpu-ffe40000_group/aml-gpu",
      "sys/platform/arm-mali-ffe40000",
      "sys/platform/audio-controller-ff642000/audio-controller-ff642000_group/aml-g12-audio-composite",
      "sys/platform/suspend",
      "sys/platform/phy-ffe09000/phy-ffe09000_group/aml_usb_phy",
      "sys/platform/phy-ffe09000/phy-ffe09000_group/aml_usb_phy/xhci",
      "sys/platform/phy-ffe09000/phy-ffe09000_group/aml_usb_phy/dwc2",
      "sys/platform/usb-ff500000/usb-ff500000_group/xhci/usb-bus",
      "sys/platform/phy-ffe09000/phy-ffe09000_group/aml_usb_phy/dwc2/usb-ff400000_group/dwc2",
      "sys/platform/phy-ffe09000/phy-ffe09000_group/aml_usb_phy/dwc2/usb-ff400000_group/dwc2/usb-peripheral",

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
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));
}

}  // namespace
