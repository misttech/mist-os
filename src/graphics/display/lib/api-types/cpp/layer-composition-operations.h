// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_LAYER_COMPOSITION_OPERATIONS_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_LAYER_COMPOSITION_OPERATIONS_H_

#include <zircon/assert.h>

#include <cstdint>

namespace display {

// TODO(https://fxbug.com/422844790): Delete this after drivers stop using it.
class LayerCompositionOperations {
 public:
  // Creates an empty subset.
  constexpr LayerCompositionOperations() noexcept : LayerCompositionOperations(0) {}

  constexpr LayerCompositionOperations(const LayerCompositionOperations&) noexcept = default;
  constexpr LayerCompositionOperations(LayerCompositionOperations&&) noexcept = default;
  constexpr LayerCompositionOperations& operator=(const LayerCompositionOperations&) noexcept =
      default;
  constexpr LayerCompositionOperations& operator=(LayerCompositionOperations&&) noexcept = default;
  ~LayerCompositionOperations() = default;

  // Raw numerical value of the equivalent FIDL value.
  //
  // This is intended to be used for developer-facing output, such as logging
  // and Inspect. The values have the same stability guarantees as the
  // equivalent FIDL type.
  constexpr uint32_t ValueForLogging() const;

  static const LayerCompositionOperations kUseImage;
  static const LayerCompositionOperations kMerge;
  static const LayerCompositionOperations kFrameScale;
  static const LayerCompositionOperations kSrcFrame;
  static const LayerCompositionOperations kTransform;
  static const LayerCompositionOperations kColorConversion;
  static const LayerCompositionOperations kAlpha;

  static const LayerCompositionOperations kNoOperations;
  static const LayerCompositionOperations kAllOperations;

  constexpr bool HasUseImage() const;
  constexpr LayerCompositionOperations WithUseImage() const;
  constexpr bool HasMerge() const;
  constexpr LayerCompositionOperations WithMerge() const;
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
  explicit constexpr LayerCompositionOperations(uint32_t operations) : operations_(operations) {}

  friend constexpr bool operator==(const LayerCompositionOperations& lhs,
                                   const LayerCompositionOperations& rhs);
  friend constexpr bool operator!=(const LayerCompositionOperations& lhs,
                                   const LayerCompositionOperations& rhs);

  uint32_t operations_;
};

constexpr bool operator==(const LayerCompositionOperations& lhs,
                          const LayerCompositionOperations& rhs) {
  return lhs.operations_ == rhs.operations_;
}

constexpr bool operator!=(const LayerCompositionOperations& lhs,
                          const LayerCompositionOperations& rhs) {
  return !(lhs == rhs);
}

constexpr uint32_t LayerCompositionOperations::ValueForLogging() const {
  return static_cast<uint32_t>(operations_);
}

inline constexpr const LayerCompositionOperations LayerCompositionOperations::kUseImage(1);
inline constexpr const LayerCompositionOperations LayerCompositionOperations::kMerge(2);
inline constexpr const LayerCompositionOperations LayerCompositionOperations::kFrameScale(4);
inline constexpr const LayerCompositionOperations LayerCompositionOperations::kSrcFrame(8);
inline constexpr const LayerCompositionOperations LayerCompositionOperations::kTransform(16);
inline constexpr const LayerCompositionOperations LayerCompositionOperations::kColorConversion(32);
inline constexpr const LayerCompositionOperations LayerCompositionOperations::kAlpha(64);

inline constexpr const LayerCompositionOperations LayerCompositionOperations::kNoOperations(0);

inline constexpr const LayerCompositionOperations LayerCompositionOperations::kAllOperations(127);

constexpr bool LayerCompositionOperations::HasUseImage() const {
  return (static_cast<uint32_t>(operations_) & static_cast<uint32_t>(kUseImage.operations_)) != 0;
}

constexpr LayerCompositionOperations LayerCompositionOperations::WithUseImage() const {
  return LayerCompositionOperations(operations_ | kUseImage.operations_);
}

constexpr bool LayerCompositionOperations::HasMerge() const {
  return (static_cast<uint32_t>(operations_) & static_cast<uint32_t>(kMerge.operations_)) != 0;
}

constexpr LayerCompositionOperations LayerCompositionOperations::WithMerge() const {
  return LayerCompositionOperations(operations_ | kMerge.operations_);
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
