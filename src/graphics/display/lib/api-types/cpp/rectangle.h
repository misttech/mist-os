// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_RECTANGLE_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_RECTANGLE_H_

#include <fidl/fuchsia.math/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>

#include <cstdint>

namespace display {

// FIDL type [`fuchsia.math/RectU`] representation useful for the display stack.
//
// Equivalent to the the banjo type [`fuchsia.hardware.display.controller/RectU`].
//
// See `::fuchsia_math::wire::RectU` for references.
//
// Instances represent rectangular axis-aligned regions inside raster images.
// The display stack uses the Vulkan coordinate space. The origin is at the
// image's top-left corner. The X axis points to the right, and the Y axis
// points downwards.
//
// Instances are guaranteed to represent regions of images whose dimensions are
// supported by the display stack.
class Rectangle {
 private:
  // Enables creating instances using the designated initializer syntax.
  struct ConstructorArgs;

 public:
  // The maximum image width supported by the display stack.
  static constexpr int kMaxImageWidth = 65535;

  // The maximum image height supported by the display stack.
  static constexpr int kMaxImageHeight = 65535;

  // True iff `fidl_rectangle` is convertible to a valid Rectangle.
  [[nodiscard]] static constexpr bool IsValid(const fuchsia_math::wire::RectU& fidl_rectangle);
  [[nodiscard]] static constexpr bool IsValid(const rect_u_t& banjo_rectangle);

  // `banjo_rectangle` must be convertible to a valid Rectangle.
  //
  // This is not a constructor to allow designated initializer syntax. Making
  // this a constructor would introduce ambiguity when designated initializer
  // syntax is used, because `rect_u_t` has the same field names as our
  // supported designated initializer syntax.
  [[nodiscard]] static constexpr Rectangle From(const rect_u_t& banjo_rectangle);

  // `fidl_rectangle` must be convertible to a valid Rectangle.
  //
  // This is not a constructor to allow designated initializer syntax. Making
  // this a constructor would introduce ambiguity when designated initializer
  // syntax is used, because `fuchsia.math/RectU` has the same field names as
  // our supported designated initializer syntax.
  [[nodiscard]] static constexpr Rectangle From(const fuchsia_math::wire::RectU& fidl_rectangle);

  // Constructor that enables the designated initializer syntax.
  //
  // NOLINTNEXTLINE(google-explicit-constructor)
  constexpr Rectangle(const Rectangle::ConstructorArgs& args);

  Rectangle(const Rectangle&) = default;
  Rectangle& operator=(const Rectangle&) = default;
  ~Rectangle() = default;

  friend constexpr bool operator==(const Rectangle& lhs, const Rectangle& rhs);
  friend constexpr bool operator!=(const Rectangle& lhs, const Rectangle& rhs);

  constexpr fuchsia_math::wire::RectU ToFidl() const;
  constexpr rect_u_t ToBanjo() const;

  // Guaranteed to be in [0, `kMaxImageWidth`].
  constexpr int32_t x() const { return x_; }

  // Guaranteed to be in [0, `kMaxImageHeight`].
  constexpr int32_t y() const { return y_; }

  // Guaranteed to be in [0, `kMaxImageWidth`].
  constexpr int32_t width() const { return width_; }

  // Guaranteed to be in [0, `kMaxImageHeight`].
  constexpr int32_t height() const { return height_; }

 private:
  struct ConstructorArgs {
    int32_t x;
    int32_t y;
    int32_t width;
    int32_t height;
  };

  // In debug mode, asserts that IsValid() would return true.
  //
  // IsValid() variant with developer-friendly debug assertions.
  static constexpr void DebugAssertIsValid(const Rectangle::ConstructorArgs& args);
  static constexpr void DebugAssertIsValid(const fuchsia_math::wire::RectU& fidl_rectangle);
  static constexpr void DebugAssertIsValid(const rect_u_t& banjo_rectangle);

  int32_t x_;
  int32_t y_;
  int32_t width_;
  int32_t height_;
};

// static
constexpr bool Rectangle::IsValid(const fuchsia_math::wire::RectU& fidl_rectangle) {
  if (fidl_rectangle.x < 0) {
    return false;
  }
  if (fidl_rectangle.x > kMaxImageWidth) {
    return false;
  }
  if (fidl_rectangle.y < 0) {
    return false;
  }
  if (fidl_rectangle.y > kMaxImageHeight) {
    return false;
  }
  if (fidl_rectangle.width < 0) {
    return false;
  }
  if (fidl_rectangle.width > kMaxImageWidth - fidl_rectangle.x) {
    return false;
  }
  if (fidl_rectangle.height < 0) {
    return false;
  }
  if (fidl_rectangle.height > kMaxImageHeight - fidl_rectangle.y) {
    return false;
  }

  return true;
}

// static
constexpr bool Rectangle::IsValid(const rect_u_t& banjo_rectangle) {
  if (banjo_rectangle.x < 0) {
    return false;
  }
  if (banjo_rectangle.x > kMaxImageWidth) {
    return false;
  }
  if (banjo_rectangle.y < 0) {
    return false;
  }
  if (banjo_rectangle.y > kMaxImageHeight) {
    return false;
  }
  if (banjo_rectangle.width < 0) {
    return false;
  }
  if (banjo_rectangle.width > kMaxImageWidth - banjo_rectangle.x) {
    return false;
  }
  if (banjo_rectangle.height < 0) {
    return false;
  }
  if (banjo_rectangle.height > kMaxImageHeight - banjo_rectangle.y) {
    return false;
  }

  return true;
}

constexpr Rectangle::Rectangle(const Rectangle::ConstructorArgs& args)
    : x_(args.x), y_(args.y), width_(args.width), height_(args.height) {
  DebugAssertIsValid(args);
}

// static
constexpr Rectangle Rectangle::From(const fuchsia_math::wire::RectU& fidl_rectangle) {
  DebugAssertIsValid(fidl_rectangle);
  return Rectangle({
      .x = static_cast<int32_t>(fidl_rectangle.x),
      .y = static_cast<int32_t>(fidl_rectangle.y),
      .width = static_cast<int32_t>(fidl_rectangle.width),
      .height = static_cast<int32_t>(fidl_rectangle.height),
  });
}

// static
constexpr Rectangle Rectangle::From(const rect_u_t& banjo_rectangle) {
  DebugAssertIsValid(banjo_rectangle);
  return Rectangle({
      .x = static_cast<int32_t>(banjo_rectangle.x),
      .y = static_cast<int32_t>(banjo_rectangle.y),
      .width = static_cast<int32_t>(banjo_rectangle.width),
      .height = static_cast<int32_t>(banjo_rectangle.height),
  });
}

constexpr bool operator==(const Rectangle& lhs, const Rectangle& rhs) {
  return lhs.x_ == rhs.x_ && lhs.y_ == rhs.y_ && lhs.width_ == rhs.width_ &&
         lhs.height_ == rhs.height_;
}

constexpr bool operator!=(const Rectangle& lhs, const Rectangle& rhs) { return !(lhs == rhs); }

constexpr fuchsia_math::wire::RectU Rectangle::ToFidl() const {
  return fuchsia_math::wire::RectU{
      // The casts are guaranteed not to overflow (causing UB) because of the
      // allowed ranges on image widths and heights.
      .x = static_cast<uint32_t>(x_),
      .y = static_cast<uint32_t>(y_),
      .width = static_cast<uint32_t>(width_),
      .height = static_cast<uint32_t>(height_),
  };
}

constexpr rect_u_t Rectangle::ToBanjo() const {
  return rect_u_t{
      // The casts are guaranteed not to overflow (causing UB) because of the
      // allowed ranges on image widths and heights.
      .x = static_cast<uint32_t>(x_),
      .y = static_cast<uint32_t>(y_),
      .width = static_cast<uint32_t>(width_),
      .height = static_cast<uint32_t>(height_),
  };
}

// static
constexpr void Rectangle::DebugAssertIsValid(const Rectangle::ConstructorArgs& args) {
  ZX_DEBUG_ASSERT(args.x >= 0);
  ZX_DEBUG_ASSERT(args.x <= Rectangle::kMaxImageWidth);
  ZX_DEBUG_ASSERT(args.y >= 0);
  ZX_DEBUG_ASSERT(args.y <= Rectangle::kMaxImageHeight);
  ZX_DEBUG_ASSERT(args.width >= 0);
  ZX_DEBUG_ASSERT(args.width <= Rectangle::kMaxImageWidth - args.x);
  ZX_DEBUG_ASSERT(args.height >= 0);
  ZX_DEBUG_ASSERT(args.height <= Rectangle::kMaxImageHeight - args.y);
}

// static
constexpr void Rectangle::DebugAssertIsValid(const fuchsia_math::wire::RectU& fidl_rectangle) {
  ZX_DEBUG_ASSERT(fidl_rectangle.x >= 0);
  ZX_DEBUG_ASSERT(fidl_rectangle.x <= Rectangle::kMaxImageWidth);
  ZX_DEBUG_ASSERT(fidl_rectangle.y >= 0);
  ZX_DEBUG_ASSERT(fidl_rectangle.y <= Rectangle::kMaxImageHeight);
  ZX_DEBUG_ASSERT(fidl_rectangle.width >= 0);
  ZX_DEBUG_ASSERT(fidl_rectangle.width <= Rectangle::kMaxImageWidth - fidl_rectangle.x);
  ZX_DEBUG_ASSERT(fidl_rectangle.height >= 0);
  ZX_DEBUG_ASSERT(fidl_rectangle.height <= Rectangle::kMaxImageHeight - fidl_rectangle.y);
}

// static
constexpr void Rectangle::DebugAssertIsValid(const rect_u_t& banjo_rectangle) {
  ZX_DEBUG_ASSERT(banjo_rectangle.x >= 0);
  ZX_DEBUG_ASSERT(banjo_rectangle.x <= Rectangle::kMaxImageWidth);
  ZX_DEBUG_ASSERT(banjo_rectangle.y >= 0);
  ZX_DEBUG_ASSERT(banjo_rectangle.y <= Rectangle::kMaxImageHeight);
  ZX_DEBUG_ASSERT(banjo_rectangle.width >= 0);
  ZX_DEBUG_ASSERT(banjo_rectangle.width <= Rectangle::kMaxImageWidth - banjo_rectangle.x);
  ZX_DEBUG_ASSERT(banjo_rectangle.height >= 0);
  ZX_DEBUG_ASSERT(banjo_rectangle.height <= Rectangle::kMaxImageHeight - banjo_rectangle.y);
}

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_RECTANGLE_H_
