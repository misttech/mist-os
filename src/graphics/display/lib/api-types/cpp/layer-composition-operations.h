// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_LAYER_COMPOSITION_OPERATIONS_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_LAYER_COMPOSITION_OPERATIONS_H_

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <zircon/assert.h>

#include <cstdint>

namespace display {

// Equivalent to the FIDL type [`fuchsia.hardware.display.engine/LayerCompositionOperations`].
//
// Also equivalent to the banjo type
// [`fuchsia.hardware.display.controller/LayerCompositionOperations`].
//
// See `::fuchsia_hardware_display_engine::wire::LayerCompositionOperations` for references.
//
// Instances are guaranteed to represent valid subsets.
class LayerCompositionOperations {
 public:
  // True iff `fidl_operations` is convertible to a valid LayerCompositionOperations.
  [[nodiscard]] static constexpr bool IsValid(
      fuchsia_hardware_display_engine::wire::LayerCompositionOperations fidl_operations);
  // True iff `banjo_transformation` is convertible to a valid LayerCompositionOperations.
  [[nodiscard]] static constexpr bool IsValid(layer_composition_operations_t banjo_operations);

  // Creates an empty subset.
  constexpr LayerCompositionOperations() noexcept = default;

  explicit constexpr LayerCompositionOperations(
      fuchsia_hardware_display_engine::wire::LayerCompositionOperations fidl_operations);
  explicit constexpr LayerCompositionOperations(layer_composition_operations_t banjo_operations);

  constexpr LayerCompositionOperations(const LayerCompositionOperations&) noexcept = default;
  constexpr LayerCompositionOperations& operator=(const LayerCompositionOperations&) noexcept =
      default;
  ~LayerCompositionOperations() = default;

  constexpr fuchsia_hardware_display_engine::wire::LayerCompositionOperations ToFidl() const;
  constexpr layer_composition_operations_t ToBanjo() const;

  // Raw numerical value of the equivalent FIDL value.
  //
  // This is intended to be used for developer-facing output, such as logging
  // and Inspect. The values have the same stability guarantees as the
  // equivalent FIDL type.
  constexpr uint32_t ValueForLogging() const;

  static const LayerCompositionOperations kUseImage;
  static const LayerCompositionOperations kMergeBase;
  static const LayerCompositionOperations kMergeSrc;
  static const LayerCompositionOperations kFrameScale;
  static const LayerCompositionOperations kSrcFrame;
  static const LayerCompositionOperations kTransform;
  static const LayerCompositionOperations kColorConversion;
  static const LayerCompositionOperations kAlpha;

  static const LayerCompositionOperations kNoOperations;
  static const LayerCompositionOperations kAllOperations;

  constexpr bool HasUseImage() const;
  constexpr LayerCompositionOperations WithUseImage() const;
  constexpr bool HasMergeBase() const;
  constexpr LayerCompositionOperations WithMergeBase() const;
  constexpr bool HasMergeSrc() const;
  constexpr LayerCompositionOperations WithMergeSrc() const;
  constexpr bool HasFrameScale() const;
  constexpr LayerCompositionOperations WithFrameScale() const;
  constexpr bool HasSrcFrame() const;
  constexpr LayerCompositionOperations WithSrcFrame() const;
  constexpr bool HasTransform() const;
  constexpr LayerCompositionOperations WithTransform() const;
  constexpr bool HasColorConversion() const;
  constexpr LayerCompositionOperations WithColorConversion() const;
  constexpr bool HasAlpha() const;
  constexpr LayerCompositionOperations WithAlpha() const;

 private:
  friend constexpr bool operator==(const LayerCompositionOperations& lhs,
                                   const LayerCompositionOperations& rhs);
  friend constexpr bool operator!=(const LayerCompositionOperations& lhs,
                                   const LayerCompositionOperations& rhs);

  fuchsia_hardware_display_engine::wire::LayerCompositionOperations operations_;
};

// static
constexpr bool LayerCompositionOperations::IsValid(
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations fidl_operations) {
  constexpr fuchsia_hardware_display_engine::wire::LayerCompositionOperations kAllOperations =
      fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kUseImage |
      fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kMergeBase |
      fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kMergeSrc |
      fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kFrameScale |
      fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kSrcFrame |
      fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kTransform |
      fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kColorConversion |
      fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kAlpha;

  return (static_cast<uint32_t>(fidl_operations) & ~static_cast<uint32_t>(kAllOperations)) == 0;
}

// static
constexpr bool LayerCompositionOperations::IsValid(
    layer_composition_operations_t banjo_operations) {
  constexpr layer_composition_operations_t kAllOperations =
      LAYER_COMPOSITION_OPERATIONS_USE_IMAGE | LAYER_COMPOSITION_OPERATIONS_MERGE_BASE |
      LAYER_COMPOSITION_OPERATIONS_MERGE_SRC | LAYER_COMPOSITION_OPERATIONS_FRAME_SCALE |
      LAYER_COMPOSITION_OPERATIONS_SRC_FRAME | LAYER_COMPOSITION_OPERATIONS_TRANSFORM |
      LAYER_COMPOSITION_OPERATIONS_COLOR_CONVERSION | LAYER_COMPOSITION_OPERATIONS_ALPHA;

  return (static_cast<uint32_t>(banjo_operations) & ~static_cast<uint32_t>(kAllOperations)) == 0;
}

constexpr LayerCompositionOperations::LayerCompositionOperations(
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations fidl_operations)
    : operations_(fidl_operations) {
  ZX_DEBUG_ASSERT(IsValid(fidl_operations));
}

constexpr LayerCompositionOperations::LayerCompositionOperations(
    layer_composition_operations_t banjo_operations)
    : operations_(static_cast<fuchsia_hardware_display_engine::wire::LayerCompositionOperations>(
          banjo_operations)) {
  ZX_DEBUG_ASSERT(IsValid(banjo_operations));
}

constexpr bool operator==(const LayerCompositionOperations& lhs,
                          const LayerCompositionOperations& rhs) {
  return lhs.operations_ == rhs.operations_;
}

constexpr bool operator!=(const LayerCompositionOperations& lhs,
                          const LayerCompositionOperations& rhs) {
  return !(lhs == rhs);
}

constexpr fuchsia_hardware_display_engine::wire::LayerCompositionOperations
LayerCompositionOperations::ToFidl() const {
  return operations_;
}

constexpr layer_composition_operations_t LayerCompositionOperations::ToBanjo() const {
  return static_cast<image_tiling_type_t>(operations_);
}

constexpr uint32_t LayerCompositionOperations::ValueForLogging() const {
  return static_cast<uint32_t>(operations_);
}

inline constexpr const LayerCompositionOperations LayerCompositionOperations::kUseImage(
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kUseImage);
inline constexpr const LayerCompositionOperations LayerCompositionOperations::kMergeBase(
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kMergeBase);
inline constexpr const LayerCompositionOperations LayerCompositionOperations::kMergeSrc(
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kMergeSrc);
inline constexpr const LayerCompositionOperations LayerCompositionOperations::kFrameScale(
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kFrameScale);
inline constexpr const LayerCompositionOperations LayerCompositionOperations::kSrcFrame(
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kSrcFrame);
inline constexpr const LayerCompositionOperations LayerCompositionOperations::kTransform(
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kTransform);
inline constexpr const LayerCompositionOperations LayerCompositionOperations::kColorConversion(
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kColorConversion);
inline constexpr const LayerCompositionOperations LayerCompositionOperations::kAlpha(
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kAlpha);

inline constexpr const LayerCompositionOperations LayerCompositionOperations::kNoOperations;

inline constexpr const LayerCompositionOperations LayerCompositionOperations::kAllOperations(
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kUseImage |
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kMergeBase |
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kMergeSrc |
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kFrameScale |
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kSrcFrame |
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kTransform |
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kColorConversion |
    fuchsia_hardware_display_engine::wire::LayerCompositionOperations::kAlpha);

constexpr bool LayerCompositionOperations::HasUseImage() const {
  return (static_cast<uint32_t>(operations_) & static_cast<uint32_t>(kUseImage.operations_)) != 0;
}

constexpr LayerCompositionOperations LayerCompositionOperations::WithUseImage() const {
  return LayerCompositionOperations(operations_ | kUseImage.operations_);
}

constexpr bool LayerCompositionOperations::HasMergeBase() const {
  return (static_cast<uint32_t>(operations_) & static_cast<uint32_t>(kMergeBase.operations_)) != 0;
}

constexpr LayerCompositionOperations LayerCompositionOperations::WithMergeBase() const {
  return LayerCompositionOperations(operations_ | kMergeBase.operations_);
}

constexpr bool LayerCompositionOperations::HasMergeSrc() const {
  return (static_cast<uint32_t>(operations_) & static_cast<uint32_t>(kMergeSrc.operations_)) != 0;
}

constexpr LayerCompositionOperations LayerCompositionOperations::WithMergeSrc() const {
  return LayerCompositionOperations(operations_ | kMergeSrc.operations_);
}

constexpr bool LayerCompositionOperations::HasFrameScale() const {
  return (static_cast<uint32_t>(operations_) & static_cast<uint32_t>(kFrameScale.operations_)) != 0;
}

constexpr LayerCompositionOperations LayerCompositionOperations::WithFrameScale() const {
  return LayerCompositionOperations(operations_ | kFrameScale.operations_);
}

constexpr bool LayerCompositionOperations::HasSrcFrame() const {
  return (static_cast<uint32_t>(operations_) & static_cast<uint32_t>(kSrcFrame.operations_)) != 0;
}

constexpr LayerCompositionOperations LayerCompositionOperations::WithSrcFrame() const {
  return LayerCompositionOperations(operations_ | kSrcFrame.operations_);
}

constexpr bool LayerCompositionOperations::HasTransform() const {
  return (static_cast<uint32_t>(operations_) & static_cast<uint32_t>(kTransform.operations_)) != 0;
}

constexpr LayerCompositionOperations LayerCompositionOperations::WithTransform() const {
  return LayerCompositionOperations(operations_ | kTransform.operations_);
}

constexpr bool LayerCompositionOperations::HasColorConversion() const {
  return (static_cast<uint32_t>(operations_) &
          static_cast<uint32_t>(kColorConversion.operations_)) != 0;
}

constexpr LayerCompositionOperations LayerCompositionOperations::WithColorConversion() const {
  return LayerCompositionOperations(operations_ | kColorConversion.operations_);
}

constexpr bool LayerCompositionOperations::HasAlpha() const {
  return (static_cast<uint32_t>(operations_) & static_cast<uint32_t>(kAlpha.operations_)) != 0;
}

constexpr LayerCompositionOperations LayerCompositionOperations::WithAlpha() const {
  return LayerCompositionOperations(operations_ | kAlpha.operations_);
}

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_LAYER_COMPOSITION_OPERATIONS_H_
