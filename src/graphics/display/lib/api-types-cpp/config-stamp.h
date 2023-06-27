// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_CONFIG_STAMP_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_CONFIG_STAMP_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>

#include <cstdint>

#include <fbl/strong_int.h>

namespace display {

// More useful representation of `fuchsia.hardware.display/ConfigStampValue`.
DEFINE_STRONG_INT(ConfigStamp, uint64_t);

constexpr inline ConfigStamp ToConfigStamp(config_stamp_t banjo_config_stamp) {
  return ConfigStamp(banjo_config_stamp.value);
}
constexpr inline ConfigStamp ToConfigStamp(
    fuchsia_hardware_display::wire::ConfigStamp fidl_config_stamp) {
  return ConfigStamp(fidl_config_stamp.value);
}
constexpr inline config_stamp_t ToBanjoConfigStamp(ConfigStamp config_stamp) {
  return config_stamp_t{.value = config_stamp.value()};
}
constexpr inline fuchsia_hardware_display::wire::ConfigStamp ToFidlConfigStamp(
    ConfigStamp config_stamp) {
  return fuchsia_hardware_display::wire::ConfigStamp{.value = config_stamp.value()};
}

constexpr ConfigStamp kInvalidConfigStamp(fuchsia_hardware_display::wire::kInvalidConfigStampValue);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_CONFIG_STAMP_H_
