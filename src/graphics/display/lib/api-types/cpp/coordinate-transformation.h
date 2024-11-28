// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_COORDINATE_TRANSFORMATION_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_COORDINATE_TRANSFORMATION_H_

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <zircon/assert.h>

namespace display {

// Equivalent to the FIDL type [`fuchsia.hardware.display.types/CoordinateTransformation`].
//
// Also equivalent to the banjo type
// [`fuchsia.hardware.display.controller/CoordinateTransformation`].
//
// See `::fuchsia_hardware_display_types::wire::CoordinateTransformation` for references.
//
// Instances are guaranteed to represent valid enum members.
class CoordinateTransformation {
 public:
  // True iff `fidl_transformation` is convertible to a valid CoordinateTransformation.
  [[nodiscard]] static constexpr bool IsValid(
      fuchsia_hardware_display_types::wire::CoordinateTransformation fidl_transformation);
  // True iff `banjo_transformation` is convertible to a valid CoordinateTransformation.
  [[nodiscard]] static constexpr bool IsValid(coordinate_transformation_t banjo_transformation);

  explicit constexpr CoordinateTransformation(
      fuchsia_hardware_display_types::wire::CoordinateTransformation fidl_transformation);

  explicit constexpr CoordinateTransformation(coordinate_transformation_t banjo_transformation);

  CoordinateTransformation(const CoordinateTransformation&) = default;
  CoordinateTransformation& operator=(const CoordinateTransformation&) = default;
  ~CoordinateTransformation() = default;

  constexpr fuchsia_hardware_display_types::wire::CoordinateTransformation ToFidl() const;
  constexpr coordinate_transformation_t ToBanjo() const;

  // Raw numerical value.
  //
  // This is intended to be used for developer-facing output, such as logging
  // and Inspect. The values are not guaranteed to have any stable semantics.
  constexpr uint32_t ValueForLogging() const;

  static const CoordinateTransformation kIdentity;
  static const CoordinateTransformation kReflectX;
  static const CoordinateTransformation kReflectY;
  static const CoordinateTransformation kRotateCcw90;
  static const CoordinateTransformation kRotateCcw180;
  static const CoordinateTransformation kRotateCcw270;
  static const CoordinateTransformation kRotateCcw90ReflectX;
  static const CoordinateTransformation kRotateCcw90ReflectY;

 private:
  friend constexpr bool operator==(const CoordinateTransformation& lhs,
                                   const CoordinateTransformation& rhs);
  friend constexpr bool operator!=(const CoordinateTransformation& lhs,
                                   const CoordinateTransformation& rhs);

  fuchsia_hardware_display_types::wire::CoordinateTransformation transformation_;
};

// static
constexpr bool CoordinateTransformation::IsValid(
    fuchsia_hardware_display_types::wire::CoordinateTransformation fidl_transformation) {
  switch (fidl_transformation) {
    case fuchsia_hardware_display_types::wire::CoordinateTransformation::kIdentity:
    case fuchsia_hardware_display_types::wire::CoordinateTransformation::kReflectX:
    case fuchsia_hardware_display_types::wire::CoordinateTransformation::kReflectY:
    case fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw180:
    case fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw90:
    case fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw90ReflectX:
    case fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw90ReflectY:
    case fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw270:
      return true;
  }
  return false;
}

// static
constexpr bool CoordinateTransformation::IsValid(coordinate_transformation_t banjo_transformation) {
  switch (banjo_transformation) {
    case COORDINATE_TRANSFORMATION_IDENTITY:
    case COORDINATE_TRANSFORMATION_REFLECT_X:
    case COORDINATE_TRANSFORMATION_REFLECT_Y:
    case COORDINATE_TRANSFORMATION_ROTATE_CCW_180:
    case COORDINATE_TRANSFORMATION_ROTATE_CCW_90:
    case COORDINATE_TRANSFORMATION_ROTATE_CCW_90_REFLECT_X:
    case COORDINATE_TRANSFORMATION_ROTATE_CCW_90_REFLECT_Y:
    case COORDINATE_TRANSFORMATION_ROTATE_CCW_270:
      return true;
  }
  return false;
}

constexpr CoordinateTransformation::CoordinateTransformation(
    fuchsia_hardware_display_types::wire::CoordinateTransformation fidl_transformation)
    : transformation_(fidl_transformation) {
  ZX_DEBUG_ASSERT(IsValid(fidl_transformation));
}

constexpr CoordinateTransformation::CoordinateTransformation(
    coordinate_transformation_t banjo_transformation)
    : transformation_(static_cast<fuchsia_hardware_display_types::wire::CoordinateTransformation>(
          banjo_transformation)) {
  ZX_DEBUG_ASSERT(IsValid(banjo_transformation));
}

constexpr bool operator==(const CoordinateTransformation& lhs,
                          const CoordinateTransformation& rhs) {
  return lhs.transformation_ == rhs.transformation_;
}

constexpr bool operator!=(const CoordinateTransformation& lhs,
                          const CoordinateTransformation& rhs) {
  return !(lhs == rhs);
}

constexpr fuchsia_hardware_display_types::wire::CoordinateTransformation
CoordinateTransformation::ToFidl() const {
  return transformation_;
}

constexpr coordinate_transformation_t CoordinateTransformation::ToBanjo() const {
  return static_cast<coordinate_transformation_t>(transformation_);
}

constexpr uint32_t CoordinateTransformation::ValueForLogging() const {
  return static_cast<uint32_t>(transformation_);
}

inline constexpr const CoordinateTransformation CoordinateTransformation::kIdentity(
    fuchsia_hardware_display_types::wire::CoordinateTransformation::kIdentity);
inline constexpr const CoordinateTransformation CoordinateTransformation::kReflectX(
    fuchsia_hardware_display_types::wire::CoordinateTransformation::kReflectX);
inline constexpr const CoordinateTransformation CoordinateTransformation::kReflectY(
    fuchsia_hardware_display_types::wire::CoordinateTransformation::kReflectY);
inline constexpr const CoordinateTransformation CoordinateTransformation::kRotateCcw90(
    fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw90);
inline constexpr const CoordinateTransformation CoordinateTransformation::kRotateCcw180(
    fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw180);
inline constexpr const CoordinateTransformation CoordinateTransformation::kRotateCcw270(
    fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw270);
inline constexpr const CoordinateTransformation CoordinateTransformation::kRotateCcw90ReflectX(
    fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw90ReflectX);
inline constexpr const CoordinateTransformation CoordinateTransformation::kRotateCcw90ReflectY(
    fuchsia_hardware_display_types::wire::CoordinateTransformation::kRotateCcw90ReflectY);

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_COORDINATE_TRANSFORMATION_H_
