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
      "sys/platform/dsi-7000/dw-dsi",
      "sys/platform/canvas-ff638000/aml-canvas",
      "sys/platform/adc-9000",
      "sys/platform/gpio-controller-ff634400/aml-gpio/gpio",
      "sys/platform/gpio-controller-ff634400/aml-gpio/gpio-init",
      "sys/platform/gpu-ffe40000/gpu-ffe40000_group/aml-gpu",
      "sys/platform/arm-mali-ffe40000",
      "sys/platform/audio-controller-ff642000/audio-controller-ff642000_group/aml-g12-audio-composite",
      "sys/platform/suspend",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));
}

}  // namespace
