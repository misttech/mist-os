// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "zircon/system/utest/device-enumeration/common.h"

namespace {

TEST_F(DeviceEnumerationTest, LuisTest) {
  static const char* kDevicePaths[] = {
      "sys/platform/05:04:13/luis-audio-pdm-in",
      "luis-i2s-audio-out",
      "sherlock-buttons/hid-buttons",

      // Thermal devices
      "sys/platform/05:04:28/thermal",

      // Thermistor and ADC devices
      "sys/platform/03:0c:27/thermistor-device/therm-mic",
      "sys/platform/03:0c:27/thermistor-device/therm-amp",
      "sys/platform/03:0c:27/thermistor-device/therm-ambient",
      "class/adc/000",
      "class/adc/001",
      "class/adc/002",
      "class/adc/003",
      "class/temperature/000",
      "class/temperature/001",
      "class/temperature/002",

      // Power Device Bucks.
      "0p8_ee_buck",
      "cpu_a_buck",

      // Power Implementation Device / Children.
      "aml-power-impl-composite",
      "composite-pd-big-core",
      "composite-pd-big-core/power-0",
      "composite-pd-little-core",
      "composite-pd-little-core/power-1",

      // CPU Device.
      // TODO(fxbug.dev/60492): Temporarily removed.
      // "sys/platform/03:0c:6",
      // "class/cpu-ctrl/000",
      // "class/cpu-ctrl/001",

      // USB ethernet; Can be RNDIS or CDC based on build config. Update this after fxbug.dev/58584
      // is fixed.
      "dwc2/dwc2/usb-peripheral/function-000",
  };

  ASSERT_NO_FATAL_FAILURE(TestRunner(kDevicePaths, std::size(kDevicePaths)));
}

}  // namespace
