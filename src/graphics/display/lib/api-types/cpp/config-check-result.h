// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_CONFIG_CHECK_RESULT_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_CONFIG_CHECK_RESULT_H_

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <zircon/assert.h>

#include <cstdint>

namespace display {

// Equivalent to the FIDL type [`fuchsia.hardware.display.types/ConfigCheckResult`].
//
// Also equivalent to the banjo type
// [`fuchsia.hardware.display.controller/ConfigCheckResult`].
//
// See `::fuchsia_hardware_display_types::wire::ConfigCheckResult` for references.
//
// Instances are guaranteed to represent valid enum members.
class ConfigCheckResult {
 public:
  // True iff `fidl_result` is convertible to a valid ConfigCheckResult.
  [[nodiscard]] static constexpr bool IsValid(
      fuchsia_hardware_display_types::wire::ConfigResult fidl_result);
  // True iff `banjo_result` is convertible to a valid ConfigCheckResult.
  [[nodiscard]] static constexpr bool IsValid(config_check_result_t banjo_result);

  explicit constexpr ConfigCheckResult(
      fuchsia_hardware_display_types::wire::ConfigResult fidl_result);

  explicit constexpr ConfigCheckResult(config_check_result_t banjo_result);

  ConfigCheckResult(const ConfigCheckResult&) = default;
  ConfigCheckResult& operator=(const ConfigCheckResult&) = default;
  ~ConfigCheckResult() = default;

  constexpr fuchsia_hardware_display_types::wire::ConfigResult ToFidl() const;
  constexpr config_check_result_t ToBanjo() const;

  // Raw numerical value of the equivalent FIDL value.
  //
  // This is intended to be used for developer-facing output, such as logging
  // and Inspect. The values have the same stability guarantees as the
  // equivalent FIDL type.
  constexpr uint32_t ValueForLogging() const;

  static const ConfigCheckResult kOk;
  static const ConfigCheckResult kInvalidConfig;
  static const ConfigCheckResult kUnsupportedConfig;
  static const ConfigCheckResult kTooManyDisplays;
  static const ConfigCheckResult kUnsupportedDisplayModes;

 private:
  friend constexpr bool operator==(const ConfigCheckResult& lhs, const ConfigCheckResult& rhs);
  friend constexpr bool operator!=(const ConfigCheckResult& lhs, const ConfigCheckResult& rhs);

  fuchsia_hardware_display_types::wire::ConfigResult result_;
};

// static
constexpr bool ConfigCheckResult::IsValid(
    fuchsia_hardware_display_types::wire::ConfigResult fidl_result) {
  switch (fidl_result) {
    case fuchsia_hardware_display_types::wire::ConfigResult::kOk:
    case fuchsia_hardware_display_types::wire::ConfigResult::kInvalidConfig:
    case fuchsia_hardware_display_types::wire::ConfigResult::kUnsupportedConfig:
    case fuchsia_hardware_display_types::wire::ConfigResult::kTooManyDisplays:
    case fuchsia_hardware_display_types::wire::ConfigResult::kUnsupportedDisplayModes:
      return true;
  }
  return false;
}

// static
constexpr bool ConfigCheckResult::IsValid(config_check_result_t banjo_result) {
  switch (banjo_result) {
    case CONFIG_CHECK_RESULT_OK:
    case CONFIG_CHECK_RESULT_INVALID_CONFIG:
    case CONFIG_CHECK_RESULT_UNSUPPORTED_CONFIG:
    case CONFIG_CHECK_RESULT_TOO_MANY:
    case CONFIG_CHECK_RESULT_UNSUPPORTED_MODES:
      return true;
  }
  return false;
}

constexpr ConfigCheckResult::ConfigCheckResult(
    fuchsia_hardware_display_types::wire::ConfigResult fidl_result)
    : result_(fidl_result) {
  ZX_DEBUG_ASSERT(IsValid(fidl_result));
}

constexpr ConfigCheckResult::ConfigCheckResult(config_check_result_t banjo_result)
    : result_(static_cast<fuchsia_hardware_display_types::wire::ConfigResult>(banjo_result)) {
  ZX_DEBUG_ASSERT(IsValid(banjo_result));
}

constexpr bool operator==(const ConfigCheckResult& lhs, const ConfigCheckResult& rhs) {
  return lhs.result_ == rhs.result_;
}

constexpr bool operator!=(const ConfigCheckResult& lhs, const ConfigCheckResult& rhs) {
  return !(lhs == rhs);
}

constexpr fuchsia_hardware_display_types::wire::ConfigResult ConfigCheckResult::ToFidl() const {
  return result_;
}

constexpr config_check_result_t ConfigCheckResult::ToBanjo() const {
  return static_cast<config_check_result_t>(result_);
}

constexpr uint32_t ConfigCheckResult::ValueForLogging() const {
  return static_cast<uint32_t>(result_);
}

inline constexpr const ConfigCheckResult ConfigCheckResult::kOk(
    fuchsia_hardware_display_types::wire::ConfigResult::kOk);
inline constexpr const ConfigCheckResult ConfigCheckResult::kInvalidConfig(
    fuchsia_hardware_display_types::wire::ConfigResult::kInvalidConfig);
inline constexpr const ConfigCheckResult ConfigCheckResult::kUnsupportedConfig(
    fuchsia_hardware_display_types::wire::ConfigResult::kUnsupportedConfig);
inline constexpr const ConfigCheckResult ConfigCheckResult::kTooManyDisplays(
    fuchsia_hardware_display_types::wire::ConfigResult::kTooManyDisplays);
inline constexpr const ConfigCheckResult ConfigCheckResult::kUnsupportedDisplayModes(
    fuchsia_hardware_display_types::wire::ConfigResult::kUnsupportedDisplayModes);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_CONFIG_CHECK_RESULT_H_
