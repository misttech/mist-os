// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_COLOR_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_COLOR_H_

#include <fidl/fuchsia.hardware.display.types/cpp/wire.h>
#include <fidl/fuchsia.images2/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <lib/stdcompat/span.h>
#include <zircon/assert.h>

#include <array>
#include <cinttypes>
#include <cstdint>

#include "src/graphics/display/lib/api-types/cpp/pixel-format.h"

namespace display {

// Equivalent to the FIDL type [`fuchsia.hardware.display.types/Color`].
//
// Equivalent to the the banjo type [`fuchsia.hardware.display.controller/Color`].
//
// Instances are guaranteed to represent color constants whose pixel formats are
// supported by the display stack.
class Color {
 private:
  // Enables creating instances using the designated initializer syntax.
  struct ConstructorArgs;

 public:
  // True iff `fidl_color` is convertible to a valid Color.
  [[nodiscard]] static constexpr bool IsValid(
      const fuchsia_hardware_display_types::wire::Color& fidl_color);
  [[nodiscard]] static constexpr bool IsValid(const color_t& banjo_color);

  // `banjo_color` must be convertible to a valid Color.
  //
  // This is not a constructor to allow designated initializer syntax. Making
  // this a constructor would introduce ambiguity when designated initializer
  // syntax is used, because `color_t` has the same field names as our supported
  // designated initializer syntax.
  [[nodiscard]] static constexpr Color From(const color_t& banjo_color);

  // `fidl_color` must be convertible to a valid Color.
  //
  // This is not a constructor to allow designated initializer syntax. Making
  // this a constructor would introduce ambiguity when designated initializer
  // syntax is used, because `fuchsia.hardware.display.types/Color` has the same
  // field names as our supported designated initializer syntax.
  [[nodiscard]] static constexpr Color From(
      const fuchsia_hardware_display_types::wire::Color& fidl_color);

  // Constructor that enables the designated initializer syntax with containers.
  //
  // NOLINTNEXTLINE(google-explicit-constructor)
  constexpr Color(const Color::ConstructorArgs& args);

  Color(const Color&) = default;
  Color& operator=(const Color&) = default;
  ~Color() = default;

  friend constexpr bool operator==(const Color& lhs, const Color& rhs);
  friend constexpr bool operator!=(const Color& lhs, const Color& rhs);

  constexpr fuchsia_hardware_display_types::wire::Color ToFidl() const;
  constexpr color_t ToBanjo() const;

  // Guaranteed to meet the requirements in the FIDL documentation.
  constexpr PixelFormat format() const { return format_; }

  // Guaranteed to meet the requirements in the FIDL documentation.
  constexpr cpp20::span<const uint8_t> bytes() const { return bytes_; }

  // True iff `format` meets the requirements in the FIDL documentation.
  static constexpr bool SupportsFormat(PixelFormat format);

 private:
  struct ConstructorArgs {
    PixelFormat format;
    cpp20::span<const uint8_t> bytes;
  };

  // In debug mode, asserts that IsValid() would return true.
  //
  // IsValid() variant with developer-friendly debug assertions.
  static constexpr void DebugAssertIsValid(const Color::ConstructorArgs& args);
  static constexpr void DebugAssertIsValid(
      const fuchsia_hardware_display_types::wire::Color& fidl_color);
  static constexpr void DebugAssertIsValid(const color_t& banjo_color);

  // Container for static_asserts on private data members.
  static void StaticAsserts();

  PixelFormat format_;
  std::array<uint8_t, 8> bytes_;

  static constexpr int kBytesElements =
      static_cast<int>(std::tuple_size_v<decltype(Color::bytes_)>);
};

// static
constexpr bool Color::SupportsFormat(PixelFormat format) {
  return format.PlaneCount() == 1 && format.EncodingSize() <= kBytesElements;
}

// static
constexpr bool Color::IsValid(const fuchsia_hardware_display_types::wire::Color& fidl_color) {
  if (!PixelFormat::IsSupported(fidl_color.format)) {
    return false;
  }
  const PixelFormat pixel_format(fidl_color.format);

  if (!Color::SupportsFormat(pixel_format)) {
    return false;
  }

  for (int i = pixel_format.EncodingSize(); i < kBytesElements; ++i) {
    if (fidl_color.bytes[i] != 0) {
      return false;
    }
  }
  return true;
}

// static
constexpr bool Color::IsValid(const color_t& banjo_color) {
  if (!PixelFormat::IsSupported(banjo_color.format)) {
    return false;
  }
  const PixelFormat pixel_format(banjo_color.format);

  if (!Color::SupportsFormat(pixel_format)) {
    return false;
  }

  for (int i = pixel_format.EncodingSize(); i < kBytesElements; ++i) {
    if (banjo_color.bytes[i] != 0) {
      return false;
    }
  }
  return true;
}

constexpr Color::Color(const Color::ConstructorArgs& args)
    : format_(args.format),
      // TODO(https://fxbug.dev/378965477): Given C++20, this error-prone code can be replaced by
      //                                    std::ranges::copy().
      bytes_({
          args.bytes.size() > 0 ? args.bytes[0] : uint8_t{0},
          args.bytes.size() > 1 ? args.bytes[1] : uint8_t{0},
          args.bytes.size() > 2 ? args.bytes[2] : uint8_t{0},
          args.bytes.size() > 3 ? args.bytes[3] : uint8_t{0},
          args.bytes.size() > 4 ? args.bytes[4] : uint8_t{0},
          args.bytes.size() > 5 ? args.bytes[5] : uint8_t{0},
          args.bytes.size() > 6 ? args.bytes[6] : uint8_t{0},
          args.bytes.size() > 7 ? args.bytes[7] : uint8_t{0},
      }) {
  DebugAssertIsValid(args);
}

// static
constexpr Color Color::From(const fuchsia_hardware_display_types::wire::Color& fidl_color) {
  DebugAssertIsValid(fidl_color);
  return Color({
      .format = PixelFormat(fidl_color.format),
      .bytes = fidl_color.bytes,
  });
}

// static
constexpr Color Color::From(const color_t& banjo_color) {
  DebugAssertIsValid(banjo_color);
  return Color({
      .format = PixelFormat(banjo_color.format),
      .bytes = banjo_color.bytes,
  });
}

constexpr bool operator==(const Color& lhs, const Color& rhs) {
  return lhs.format_ == rhs.format_ && lhs.bytes_ == rhs.bytes_;
}

constexpr bool operator!=(const Color& lhs, const Color& rhs) { return !(lhs == rhs); }

constexpr fuchsia_hardware_display_types::wire::Color Color::ToFidl() const {
  fuchsia_hardware_display_types::wire::Color fidl_color{.format = format_.ToFidl()};
  // TODO(https://fxbug.dev/378965477): Given C++20, this error-prone code can be replaced by
  //                                    std::ranges::copy().
  for (int i = 0; i < Color::kBytesElements; ++i) {
    fidl_color.bytes[i] = bytes_[i];
  }
  return fidl_color;
}

constexpr color_t Color::ToBanjo() const {
  color_t banjo_color{.format = format_.ToBanjo()};
  // TODO(https://fxbug.dev/378965477): Given C++20, this error-prone code can be replaced by
  //                                    std::ranges::copy().
  for (int i = 0; i < Color::kBytesElements; ++i) {
    banjo_color.bytes[i] = bytes_[i];
  }
  return banjo_color;
}

// static
constexpr void Color::DebugAssertIsValid(const Color::ConstructorArgs& args) {
  ZX_DEBUG_ASSERT_MSG(Color::SupportsFormat(args.format), "Unsupported color format %u" PRIu32,
                      args.format.ValueForLogging());

  for (int i = args.format.EncodingSize(); i < kBytesElements; ++i) {
    ZX_DEBUG_ASSERT_MSG(args.bytes[i] == 0, "Padding byte %d set to %d", i, int{args.bytes[i]});
  }
}

// static
constexpr void Color::DebugAssertIsValid(
    const fuchsia_hardware_display_types::wire::Color& fidl_color) {
  ZX_DEBUG_ASSERT(PixelFormat::IsSupported(fidl_color.format));
  const PixelFormat pixel_format(fidl_color.format);

  ZX_DEBUG_ASSERT_MSG(Color::SupportsFormat(pixel_format), "Unsupported color format %" PRIu32,
                      pixel_format.ValueForLogging());

  for (int i = pixel_format.EncodingSize(); i < kBytesElements; ++i) {
    ZX_DEBUG_ASSERT_MSG(fidl_color.bytes[i] == 0, "Padding byte %d set to %d", i,
                        int{fidl_color.bytes[i]});
  }
}

// static
constexpr void Color::DebugAssertIsValid(const color_t& banjo_color) {
  ZX_DEBUG_ASSERT(PixelFormat::IsSupported(banjo_color.format));
  const PixelFormat pixel_format(banjo_color.format);

  ZX_DEBUG_ASSERT_MSG(Color::SupportsFormat(pixel_format), "Unsupported color format %" PRIu32,
                      pixel_format.ValueForLogging());

  for (int i = pixel_format.EncodingSize(); i < kBytesElements; ++i) {
    ZX_DEBUG_ASSERT_MSG(banjo_color.bytes[i] == 0, "Padding byte %d set to %d", i,
                        int{banjo_color.bytes[i]});
  }
}

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_COLOR_H_
