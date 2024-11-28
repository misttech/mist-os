// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DIMENSIONS_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DIMENSIONS_H_

#include <zircon/assert.h>

#include <cstdint>

namespace display {

// The size of a rectangular region of pixels within a raster image.
//
// Similar to the VkExtent2D concept in the Vulkan API.
//
// See `::fuchsia_math::wire::RectU` for references.
//
// Instances are guaranteed to represent dimensions supported by the display
// stack.
//
// The canonical representation of empty region dimensions is 0x0 (width and
// height are both zero). Instances are guaranteed to only use the canonical
// representation for empty region dimensions.
class Dimensions {
 private:
  // Enables creating instances using the designated initializer syntax.
  struct ConstructorArgs;
  struct ConstructorArgsUnsigned;

 public:
  // The maximum image width supported by the display stack.
  static constexpr int kMaxWidth = 65535;

  // The maximum image height supported by the display stack.
  static constexpr int kMaxHeight = 65535;

  // True iff the arguments would make up a valid Dimensions instance.
  [[nodiscard]] static constexpr bool IsValid(const ConstructorArgs& args);

  // True iff the arguments would make up a valid Dimensions instance.
  //
  // Provided to facilitate validating FIDL / Banjo API inputs.
  [[nodiscard]] static constexpr bool IsValid(const ConstructorArgsUnsigned& args);

  // Constructor that enables the designated initializer syntax.
  //
  // NOLINTNEXTLINE(google-explicit-constructor)
  constexpr Dimensions(const Dimensions::ConstructorArgs& args);

  // Constructor for FIDL / Banjo API inputs.
  //
  // NOLINTNEXTLINE(google-explicit-constructor)
  constexpr Dimensions(const Dimensions::ConstructorArgsUnsigned& args);

  Dimensions(const Dimensions&) = default;
  Dimensions& operator=(const Dimensions&) = default;
  ~Dimensions() = default;

  friend constexpr bool operator==(const Dimensions& lhs, const Dimensions& rhs);
  friend constexpr bool operator!=(const Dimensions& lhs, const Dimensions& rhs);

  // Guaranteed to be in [0, `kMaxWidth`].
  constexpr int32_t width() const { return width_; }

  // Guaranteed to be in [0, `kMaxHeight`].
  constexpr int32_t height() const { return height_; }

  // True iff the dimensions represent an empty region.
  constexpr bool IsEmpty() const { return width_ == 0; }

 private:
  struct ConstructorArgs {
    int32_t width;
    int32_t height;
  };

  // For constructing from FIDL / Banjo API inputs.
  //
  // The field names must differ from `ConstructorArgs` to avoid designated
  // initializer ambiguity errors.
  struct ConstructorArgsUnsigned {
    uint32_t unsigned_width;
    uint32_t unsigned_height;
  };

  int32_t width_;
  int32_t height_;
};

// static
constexpr bool Dimensions::IsValid(const ConstructorArgs& args) {
  if (args.width < 0 || args.width > kMaxWidth) {
    return false;
  }
  if (args.height < 0 || args.height > kMaxHeight) {
    return false;
  }

  const bool width_is_zero = (args.width == 0);
  const bool height_is_zero = (args.height == 0);
  return width_is_zero == height_is_zero;
}

// static
constexpr bool Dimensions::IsValid(const ConstructorArgsUnsigned& args) {
  if (args.unsigned_width > kMaxWidth) {
    return false;
  }
  if (args.unsigned_height > kMaxHeight) {
    return false;
  }

  const bool width_is_zero = (args.unsigned_width == 0);
  const bool height_is_zero = (args.unsigned_height == 0);
  return width_is_zero == height_is_zero;
}

constexpr Dimensions::Dimensions(const Dimensions::ConstructorArgs& args)
    : width_(args.width), height_(args.height) {
  ZX_DEBUG_ASSERT(args.width >= 0);
  ZX_DEBUG_ASSERT(args.width <= kMaxWidth);
  ZX_DEBUG_ASSERT(args.height >= 0);
  ZX_DEBUG_ASSERT(args.height <= kMaxHeight);
  ZX_DEBUG_ASSERT((args.width == 0) == (args.height == 0));
}

constexpr Dimensions::Dimensions(const Dimensions::ConstructorArgsUnsigned& args)
    : width_(static_cast<int32_t>(args.unsigned_width)),
      height_(static_cast<int32_t>(args.unsigned_height)) {
  ZX_DEBUG_ASSERT(args.unsigned_width <= kMaxWidth);
  ZX_DEBUG_ASSERT(args.unsigned_height <= kMaxHeight);
  ZX_DEBUG_ASSERT((args.unsigned_width == 0) == (args.unsigned_height == 0));
}

constexpr bool operator==(const Dimensions& lhs, const Dimensions& rhs) {
  return lhs.width_ == rhs.width_ && lhs.height_ == rhs.height_;
}

constexpr bool operator!=(const Dimensions& lhs, const Dimensions& rhs) { return !(lhs == rhs); }

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_DIMENSIONS_H_
