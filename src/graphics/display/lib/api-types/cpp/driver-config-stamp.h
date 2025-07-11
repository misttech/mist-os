// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_CONFIG_STAMP_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_CONFIG_STAMP_H_

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include "src/graphics/display/lib/api-types/cpp/id-type.h"

namespace display::internal {

struct DriverConfigStampTraits
    : public display::internal::DefaultIdTypeTraits<
          uint64_t, fuchsia_hardware_display_engine::wire::ConfigStamp, config_stamp_t> {
  static constexpr uint64_t FromBanjo(const config_stamp_t& banjo_config_stamp) noexcept {
    return banjo_config_stamp.value;
  }
  static constexpr config_stamp_t ToBanjo(const uint64_t& config_stamp_value) noexcept {
    return config_stamp_t{.value = config_stamp_value};
  }
};

}  // namespace display::internal

namespace display {

// More useful representation of `fuchsia.hardware.display.engine/ConfigStamp`.
using DriverConfigStamp = display::internal::IdType<display::internal::DriverConfigStampTraits>;

constexpr DriverConfigStamp ToDriverConfigStamp(const config_stamp_t& banjo_driver_config_stamp) {
  return DriverConfigStamp(banjo_driver_config_stamp);
}
constexpr DriverConfigStamp ToDriverConfigStamp(
    fuchsia_hardware_display_engine::wire::ConfigStamp fidl_driver_config_stamp) {
  return DriverConfigStamp(fidl_driver_config_stamp);
}
constexpr config_stamp_t ToBanjoDriverConfigStamp(DriverConfigStamp driver_config_stamp) {
  return driver_config_stamp.ToBanjo();
}
constexpr fuchsia_hardware_display_engine::wire::ConfigStamp ToFidlDriverConfigStamp(
    DriverConfigStamp driver_config_stamp) {
  return driver_config_stamp.ToFidl();
}

constexpr DriverConfigStamp kInvalidDriverConfigStamp(
    fuchsia_hardware_display_engine::wire::kInvalidConfigStampValue);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DRIVER_CONFIG_STAMP_H_
