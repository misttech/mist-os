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

#include <algorithm>
#include <array>
#include <cinttypes>
#include <cstdint>

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
  constexpr fuchsia_images2::wire::PixelFormat format() const { return format_; }

  // Guaranteed to meet the requirements in the FIDL documentation.
  constexpr cpp20::span<const uint8_t> bytes() const { return bytes_; }

  // True iff `format` meets the requirements in the FIDL documentation.
  static constexpr bool SupportsFormat(fuchsia_images2::wire::PixelFormat format);

  // The number of bytes it takes to encode one pixel using `format`.
  //
  // `format` must meet the FIDL documentation requirements.
  static constexpr int EncodingSize(fuchsia_images2::wire::PixelFormat format);

 private:
  struct ConstructorArgs {
    fuchsia_images2::wire::PixelFormat format;
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
  void StaticAsserts();

  fuchsia_images2::PixelFormat format_;
  std::array<uint8_t, 8> bytes_;
};

// static
constexpr bool Color::SupportsFormat(fuchsia_images2::wire::PixelFormat format) {
  switch (format) {
    case fuchsia_images2::PixelFormat::kR8G8B8A8:
    case fuchsia_images2::PixelFormat::kB8G8R8A8:
    case fuchsia_images2::PixelFormat::kB8G8R8:
    case fuchsia_images2::PixelFormat::kR5G6B5:
    case fuchsia_images2::PixelFormat::kR3G3B2:
    case fuchsia_images2::PixelFormat::kL8:
    case fuchsia_images2::PixelFormat::kR8:
    case fuchsia_images2::PixelFormat::kA2R10G10B10:
    case fuchsia_images2::PixelFormat::kA2B10G10R10:
    case fuchsia_images2::PixelFormat::kR8G8B8:
      return true;

    default:
      return false;
  }
}

// static
constexpr int Color::EncodingSize(fuchsia_images2::wire::PixelFormat format) {
  switch (format) {
    case fuchsia_images2::PixelFormat::kR8G8B8A8:
    case fuchsia_images2::PixelFormat::kB8G8R8A8:
      return 4;

    case fuchsia_images2::PixelFormat::kB8G8R8:
      return 3;

    case fuchsia_images2::PixelFormat::kR5G6B5:
      return 2;

    case fuchsia_images2::PixelFormat::kR3G3B2:
    case fuchsia_images2::PixelFormat::kL8:
    case fuchsia_images2::PixelFormat::kR8:
      return 1;

    case fuchsia_images2::PixelFormat::kA2R10G10B10:
    case fuchsia_images2::PixelFormat::kA2B10G10R10:
      return 4;

    case fuchsia_images2::PixelFormat::kR8G8B8:
      return 3;

    default:
      ZX_DEBUG_ASSERT_MSG(false, "Unsupported format: %" PRIu32, static_cast<uint32_t>(format));
      return 0;
  }
}

// static
constexpr bool Color::IsValid(const fuchsia_hardware_display_types::wire::Color& fidl_color) {
  if (!Color::SupportsFormat(fidl_color.format)) {
    return false;
  }

  constexpr int kBytesFieldSize = static_cast<int>(sizeof(fidl_color.bytes));
  for (int i = Color::EncodingSize(fidl_color.format); i < kBytesFieldSize; ++i) {
    if (fidl_color.bytes[i] != 0) {
      return false;
    }
  }
  return true;
}

// static
constexpr bool Color::IsValid(const color_t& banjo_color) {
  const auto fidl_format = static_cast<fuchsia_images2::wire::PixelFormat>(banjo_color.format);
  if (!Color::SupportsFormat(fidl_format)) {
    return false;
  }

  constexpr int kBytesFieldSize = static_cast<int>(sizeof(banjo_color.bytes));
  for (int i = Color::EncodingSize(fidl_format); i < kBytesFieldSize; ++i) {
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
      .format = fidl_color.format,
      .bytes = fidl_color.bytes,
  });
}

// static
constexpr Color Color::From(const color_t& banjo_color) {
  DebugAssertIsValid(banjo_color);
  return Color({
      .format = static_cast<fuchsia_images2::PixelFormat>(banjo_color.format),
      .bytes = banjo_color.bytes,
  });
}

constexpr bool operator==(const Color& lhs, const Color& rhs) {
  return lhs.format_ == rhs.format_ && lhs.bytes_ == rhs.bytes_;
}

constexpr bool operator!=(const Color& lhs, const Color& rhs) { return !(lhs == rhs); }

constexpr fuchsia_hardware_display_types::wire::Color Color::ToFidl() const {
  return fuchsia_hardware_display_types::wire::Color{
      .format = format_,
      // TODO(https://fxbug.dev/378965477): Given C++20, this error-prone code can be replaced by
      //                                    std::ranges::copy().
      .bytes = {bytes_[0], bytes_[1], bytes_[2], bytes_[3], bytes_[4], bytes_[5], bytes_[6],
                bytes_[7]},
  };
}

constexpr color_t Color::ToBanjo() const {
  return color_t{
      .format = static_cast<uint32_t>(format_),
      // TODO(https://fxbug.dev/378965477): Given C++20, this error-prone code can be replaced by
      //                                    std::ranges::copy().
      .bytes = {bytes_[0], bytes_[1], bytes_[2], bytes_[3], bytes_[4], bytes_[5], bytes_[6],
                bytes_[7]},
  };
}

// static
constexpr void Color::DebugAssertIsValid(const Color::ConstructorArgs& args) {
  ZX_DEBUG_ASSERT_MSG(Color::SupportsFormat(args.format), "Unsupported color format %u" PRIu32,
                      static_cast<uint32_t>(args.format));

  int bytes_size = static_cast<int>(args.bytes.size());
  for (int i = Color::EncodingSize(args.format); i < bytes_size; ++i) {
    ZX_DEBUG_ASSERT_MSG(args.bytes[i] == 0, "Padding byte %d set to %d", i, int{args.bytes[i]});
  }
}

// static
constexpr void Color::DebugAssertIsValid(
    const fuchsia_hardware_display_types::wire::Color& fidl_color) {
  ZX_DEBUG_ASSERT_MSG(Color::SupportsFormat(fidl_color.format), "Unsupported color format %" PRIu32,
                      static_cast<uint32_t>(fidl_color.format));

  constexpr int kBytesFieldSize = static_cast<int>(sizeof(fidl_color.bytes));
  for (int i = Color::EncodingSize(fidl_color.format); i < kBytesFieldSize; ++i) {
    ZX_DEBUG_ASSERT_MSG(fidl_color.bytes[i] == 0, "Padding byte %d set to %d", i,
                        int{fidl_color.bytes[i]});
  }
}

// static
constexpr void Color::DebugAssertIsValid(const color_t& banjo_color) {
  const auto format = static_cast<fuchsia_images2::wire::PixelFormat>(banjo_color.format);
  ZX_DEBUG_ASSERT_MSG(Color::SupportsFormat(format), "Unsupported color format %" PRIu32,
                      banjo_color.format);

  constexpr int kBytesFieldSize = static_cast<int>(sizeof(banjo_color.bytes));
  for (int i = Color::EncodingSize(format); i < kBytesFieldSize; ++i) {
    ZX_DEBUG_ASSERT_MSG(banjo_color.bytes[i] == 0, "Padding byte %d set to %d", i,
                        int{banjo_color.bytes[i]});
  }
}

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_COLOR_H_
