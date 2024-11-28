// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_ALPHA_MODE_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_ALPHA_MODE_H_

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <zircon/assert.h>

namespace display {

// Equivalent to the FIDL type [`fuchsia.hardware.display.types/AlphaMode`].
//
// Also equivalent to the banjo type [`fuchsia.hardware.display.controller/Alpha`].
//
// See `::fuchsia_hardware_display_types::wire::AlphaMode` for references.
//
// Instances are guaranteed to represent valid enum members.
//
// Out-of-tree drivers must not use this interface, because it will be reworked.
class AlphaMode {
 public:
  // True iff `fidl_alpha_mode` is convertible to a valid AlphaMode.
  [[nodiscard]] static constexpr bool IsValid(
      fuchsia_hardware_display_types::wire::AlphaMode fidl_alpha_mode);
  // True iff `banjo_alpha_mode` is convertible to a valid AlphaMode.
  [[nodiscard]] static constexpr bool IsValid(alpha_t banjo_alpha_mode);

  explicit constexpr AlphaMode(fuchsia_hardware_display_types::wire::AlphaMode fidl_alpha_mode);

  explicit constexpr AlphaMode(alpha_t banjo_alpha_mode);

  AlphaMode(const AlphaMode&) = default;
  AlphaMode& operator=(const AlphaMode&) = default;
  ~AlphaMode() = default;

  constexpr fuchsia_hardware_display_types::wire::AlphaMode ToFidl() const;
  constexpr alpha_t ToBanjo() const;

  // Raw numerical value.
  //
  // This is intended to be used for developer-facing output, such as logging
  // and Inspect. The values are not guaranteed to have any stable semantics.
  constexpr uint32_t ValueForLogging() const;

  static const AlphaMode kDisable;
  static const AlphaMode kHwMultiply;
  static const AlphaMode kPremultiplied;

 private:
  friend constexpr bool operator==(const AlphaMode& lhs, const AlphaMode& rhs);
  friend constexpr bool operator!=(const AlphaMode& lhs, const AlphaMode& rhs);

  fuchsia_hardware_display_types::wire::AlphaMode alpha_mode_;
};

// static
constexpr bool AlphaMode::IsValid(fuchsia_hardware_display_types::wire::AlphaMode fidl_alpha_mode) {
  switch (fidl_alpha_mode) {
    case fuchsia_hardware_display_types::wire::AlphaMode::kDisable:
    case fuchsia_hardware_display_types::wire::AlphaMode::kPremultiplied:
    case fuchsia_hardware_display_types::wire::AlphaMode::kHwMultiply:
      return true;
  }
  return false;
}

// static
constexpr bool AlphaMode::IsValid(alpha_t banjo_alpha_mode) {
  switch (banjo_alpha_mode) {
    case ALPHA_DISABLE:
    case ALPHA_PREMULTIPLIED:
    case ALPHA_HW_MULTIPLY:
      return true;
  }
  return false;
}

constexpr AlphaMode::AlphaMode(fuchsia_hardware_display_types::wire::AlphaMode fidl_alpha_mode)
    : alpha_mode_(fidl_alpha_mode) {
  ZX_DEBUG_ASSERT(IsValid(fidl_alpha_mode));
}

constexpr AlphaMode::AlphaMode(alpha_t banjo_alpha_mode)
    : alpha_mode_(static_cast<fuchsia_hardware_display_types::wire::AlphaMode>(banjo_alpha_mode)) {
  ZX_DEBUG_ASSERT(IsValid(banjo_alpha_mode));
}

constexpr bool operator==(const AlphaMode& lhs, const AlphaMode& rhs) {
  return lhs.alpha_mode_ == rhs.alpha_mode_;
}

constexpr bool operator!=(const AlphaMode& lhs, const AlphaMode& rhs) { return !(lhs == rhs); }

constexpr fuchsia_hardware_display_types::wire::AlphaMode AlphaMode::ToFidl() const {
  return alpha_mode_;
}

constexpr alpha_t AlphaMode::ToBanjo() const { return static_cast<alpha_t>(alpha_mode_); }

constexpr uint32_t AlphaMode::ValueForLogging() const { return static_cast<uint32_t>(alpha_mode_); }

inline constexpr const AlphaMode AlphaMode::kDisable(
    fuchsia_hardware_display_types::wire::AlphaMode::kDisable);
inline constexpr const AlphaMode AlphaMode::kPremultiplied(
    fuchsia_hardware_display_types::wire::AlphaMode::kPremultiplied);
inline constexpr const AlphaMode AlphaMode::kHwMultiply(
    fuchsia_hardware_display_types::wire::AlphaMode::kHwMultiply);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_ALPHA_MODE_H_
