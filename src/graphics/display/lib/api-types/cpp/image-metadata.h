// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_IMAGE_METADATA_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_IMAGE_METADATA_H_

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <zircon/assert.h>

#include <cstdint>

#include "src/graphics/display/lib/api-types/cpp/image-tiling-type.h"

namespace display {

// Equivalent to the FIDL type [`fuchsia.hardware.display.types/ImageMetadata`].
//
// Instances are guaranteed to represent images whose dimensions are supported
// by the display stack.
class ImageMetadata {
 private:
  // Enables creating instances using the designated initializer syntax.
  struct ConstructorArgs;

 public:
  // The maximum image width supported by the display stack.
  static constexpr int kMaxWidth = 65535;

  // The maximum image height supported by the display stack.
  static constexpr int kMaxHeight = 65535;

  // True iff `image_metadata` is convertible to a valid ImageMetadata.
  [[nodiscard]] static constexpr bool IsValid(
      const fuchsia_hardware_display_types::wire::ImageMetadata& fidl_image_metadata);
  [[nodiscard]] static constexpr bool IsValid(const image_metadata_t& banjo_image_metadata);

  // Constructor that enables the designated initializer syntax.
  //
  // NOLINTNEXTLINE(google-explicit-constructor)
  constexpr ImageMetadata(const ImageMetadata::ConstructorArgs& args);

  // `fidl_image_metadata` must be convertible to a valid ImageMetadata.
  explicit constexpr ImageMetadata(
      const fuchsia_hardware_display_types::wire::ImageMetadata& fidl_image_metadata);

  // `banjo_image_metadata` must be convertible to a valid ImageMetadata.
  explicit constexpr ImageMetadata(const image_metadata_t& banjo_image_metadata);

  ImageMetadata(const ImageMetadata&) = default;
  ImageMetadata& operator=(const ImageMetadata&) = default;
  ~ImageMetadata() = default;

  friend constexpr bool operator==(const ImageMetadata& lhs, const ImageMetadata& rhs);
  friend constexpr bool operator!=(const ImageMetadata& lhs, const ImageMetadata& rhs);

  constexpr fuchsia_hardware_display_types::wire::ImageMetadata ToFidl() const;
  constexpr image_metadata_t ToBanjo() const;

  // Guaranteed to be in [0, `kMaxWidth`].
  constexpr int32_t width() const { return width_; }

  // Guaranteed to be in [0, `kMaxHeight`].
  constexpr int32_t height() const { return height_; }

  constexpr ImageTilingType tiling_type() const { return tiling_type_; }

 private:
  struct ConstructorArgs {
    int32_t width;
    int32_t height;
    ImageTilingType tiling_type;
  };

  // In debug mode, asserts that IsValid() would return true.
  //
  // IsValid() variant with developer-friendly debug assertions.
  static constexpr void DebugAssertIsValid(const ImageMetadata::ConstructorArgs& args);
  static constexpr void DebugAssertIsValid(
      const fuchsia_hardware_display_types::wire::ImageMetadata& fidl_image_metadata);
  static constexpr void DebugAssertIsValid(const image_metadata_t& banjo_image_metadata);

  int32_t width_;
  int32_t height_;
  ImageTilingType tiling_type_;
};

// static
constexpr bool ImageMetadata::IsValid(
    const fuchsia_hardware_display_types::wire::ImageMetadata& fidl_image_metadata) {
  if (fidl_image_metadata.width < 0) {
    return false;
  }
  if (fidl_image_metadata.width > kMaxWidth) {
    return false;
  }
  if (fidl_image_metadata.height < 0) {
    return false;
  }
  if (fidl_image_metadata.height > kMaxWidth) {
    return false;
  }

  return true;
}

// static
constexpr bool ImageMetadata::IsValid(const image_metadata_t& banjo_image_metadata) {
  if (banjo_image_metadata.width < 0) {
    return false;
  }
  if (banjo_image_metadata.width > kMaxWidth) {
    return false;
  }
  if (banjo_image_metadata.height < 0) {
    return false;
  }
  if (banjo_image_metadata.height > kMaxWidth) {
    return false;
  }

  return true;
}

constexpr ImageMetadata::ImageMetadata(const ImageMetadata::ConstructorArgs& args)
    : width_(args.width), height_(args.height), tiling_type_(args.tiling_type) {
  DebugAssertIsValid(args);
}

constexpr ImageMetadata::ImageMetadata(
    const fuchsia_hardware_display_types::wire::ImageMetadata& fidl_image_metadata)
    : width_(static_cast<int32_t>(fidl_image_metadata.width)),
      height_(static_cast<int32_t>(fidl_image_metadata.height)),
      tiling_type_(fidl_image_metadata.tiling_type) {
  DebugAssertIsValid(fidl_image_metadata);
}

constexpr ImageMetadata::ImageMetadata(const image_metadata_t& banjo_image_metadata)
    : width_(static_cast<int32_t>(banjo_image_metadata.width)),
      height_(static_cast<int32_t>(banjo_image_metadata.height)),
      tiling_type_(banjo_image_metadata.tiling_type) {
  DebugAssertIsValid(banjo_image_metadata);
}

constexpr bool operator==(const ImageMetadata& lhs, const ImageMetadata& rhs) {
  return lhs.width_ == rhs.width_ && lhs.height_ == rhs.height_ &&
         lhs.tiling_type_ == rhs.tiling_type_;
}

constexpr bool operator!=(const ImageMetadata& lhs, const ImageMetadata& rhs) {
  return !(lhs == rhs);
}

constexpr fuchsia_hardware_display_types::wire::ImageMetadata ImageMetadata::ToFidl() const {
  return fuchsia_hardware_display_types::wire::ImageMetadata{
      // The casts are guaranteed not to overflow (causing UB) because of the
      // allowed ranges on image widths and heights.
      .width = static_cast<uint32_t>(width_),
      .height = static_cast<uint32_t>(height_),
      .tiling_type = tiling_type_.ToFidl(),
  };
}

constexpr image_metadata_t ImageMetadata::ToBanjo() const {
  return image_metadata_t{
      // The casts are guaranteed not to overflow (causing UB) because of the
      // allowed ranges on image widths and heights.
      .width = static_cast<uint32_t>(width_),
      .height = static_cast<uint32_t>(height_),
      .tiling_type = tiling_type_.ToBanjo(),
  };
}

// static
constexpr void ImageMetadata::DebugAssertIsValid(const ImageMetadata::ConstructorArgs& args) {
  ZX_DEBUG_ASSERT(args.width >= 0);
  ZX_DEBUG_ASSERT(args.width <= ImageMetadata::kMaxWidth);
  ZX_DEBUG_ASSERT(args.height >= 0);
  ZX_DEBUG_ASSERT(args.height <= ImageMetadata::kMaxHeight);
}

// static
constexpr void ImageMetadata::DebugAssertIsValid(
    const fuchsia_hardware_display_types::wire::ImageMetadata& fidl_image_metadata) {
  ZX_DEBUG_ASSERT(fidl_image_metadata.width >= 0);
  ZX_DEBUG_ASSERT(fidl_image_metadata.width <= ImageMetadata::kMaxWidth);
  ZX_DEBUG_ASSERT(fidl_image_metadata.height >= 0);
  ZX_DEBUG_ASSERT(fidl_image_metadata.height <= ImageMetadata::kMaxHeight);
}

// static
constexpr void ImageMetadata::DebugAssertIsValid(const image_metadata_t& banjo_image_metadata) {
  ZX_DEBUG_ASSERT(banjo_image_metadata.width >= 0);
  ZX_DEBUG_ASSERT(banjo_image_metadata.width <= ImageMetadata::kMaxWidth);
  ZX_DEBUG_ASSERT(banjo_image_metadata.height >= 0);
  ZX_DEBUG_ASSERT(banjo_image_metadata.height <= ImageMetadata::kMaxHeight);
}

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_IMAGE_METADATA_H_
