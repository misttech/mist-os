// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_CONFIG_STAMP_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_CONFIG_STAMP_H_

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <fbl/strong_int.h>

namespace display {

// More useful representation of `fuchsia.hardware.display.engine/ConfigStamp`.
DEFINE_STRONG_INT(DriverConfigStamp, uint64_t);

constexpr DriverConfigStamp ToDriverConfigStamp(config_stamp_t banjo_driver_config_stamp) {
  return DriverConfigStamp(banjo_driver_config_stamp.value);
}
constexpr DriverConfigStamp ToDriverConfigStamp(
    fuchsia_hardware_display_engine::wire::ConfigStamp fidl_driver_config_stamp) {
  return DriverConfigStamp(fidl_driver_config_stamp.value);
}
constexpr config_stamp_t ToBanjoDriverConfigStamp(DriverConfigStamp driver_config_stamp) {
  return config_stamp_t{.value = driver_config_stamp.value()};
}
constexpr fuchsia_hardware_display_engine::wire::ConfigStamp ToFidlDriverConfigStamp(
    DriverConfigStamp driver_config_stamp) {
  return fuchsia_hardware_display_engine::wire::ConfigStamp{.value = driver_config_stamp.value()};
}

constexpr DriverConfigStamp kInvalidDriverConfigStamp(
    fuchsia_hardware_display_engine::wire::kInvalidConfigStampValue);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_CONFIG_STAMP_H_
