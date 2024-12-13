// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_MODE_H_
#define SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_MODE_H_

#include <fidl/fuchsia.hardware.display/cpp/wire.h>
#include <fuchsia/hardware/display/controller/cpp/banjo.h>
#include <zircon/assert.h>

#include <cinttypes>
#include <cstdint>

#include "src/graphics/display/lib/api-types/cpp/dimensions.h"

namespace display {

// Equivalent to the FIDL type [`fuchsia.hardware.display.types/Mode`].
//
// Instances are guaranteed to represent display modes supported by the display
// stack.
class Mode {
 private:
  // Enables creating instances using the designated initializer syntax.
  struct ConstructorArgs;

 public:
  // The maximum refresh rate supported by the display stack.
  static constexpr int32_t kMaxRefreshRateMillihertz = 1'000'000;  // 1,000 Hz

  // True iff `fidl_mode` is convertible to a valid Mode.
  [[nodiscard]] static constexpr bool IsValid(
      const fuchsia_hardware_display::wire::Mode& fidl_mode);
  [[nodiscard]] static constexpr bool IsValid(const display_mode_t& banjo_mode);

  // `banjo_mode` must be convertible to a valid Mode.
  //
  // This is not a constructor to allow designated initializer syntax. Making
  // this a constructor would introduce ambiguity when designated initializer
  // syntax is used, because `display_mode_t` has the same field names as our
  // supported designated initializer syntax.
  [[nodiscard]] static constexpr Mode From(const display_mode_t& banjo_mode);

  // `fidl_mode` must be convertible to a valid Mode.
  //
  // This is not a constructor to allow designated initializer syntax. Making
  // this a constructor would introduce ambiguity when designated initializer
  // syntax is used, because `fuchsia.hardware.display.types/Mode` has the same
  // field names as our supported designated initializer syntax.
  [[nodiscard]] static constexpr Mode From(const fuchsia_hardware_display::wire::Mode& fidl_mode);

  // Constructor that enables the designated initializer syntax with containers.
  //
  // NOLINTNEXTLINE(google-explicit-constructor)
  constexpr Mode(const Mode::ConstructorArgs& args);

  Mode(const Mode&) = default;
  Mode& operator=(const Mode&) = default;
  ~Mode() = default;

  friend constexpr bool operator==(const Mode& lhs, const Mode& rhs);
  friend constexpr bool operator!=(const Mode& lhs, const Mode& rhs);

  constexpr fuchsia_hardware_display::wire::Mode ToFidl() const;
  constexpr display_mode_t ToBanjo() const;

  // Guaranteed to meet the requirements in the FIDL documentation.
  constexpr Dimensions active_area() const { return active_area_; }

  // Guaranteed to meet the requirements in the FIDL documentation.
  constexpr int32_t refresh_rate_millihertz() const { return refresh_rate_millihertz_; }

 private:
  struct ConstructorArgs {
    int32_t active_width;
    int32_t active_height;
    int32_t refresh_rate_millihertz;
  };

  // In debug mode, asserts that IsValid() would return true.
  //
  // IsValid() variant with developer-friendly debug assertions.
  static constexpr void DebugAssertIsValid(const Mode::ConstructorArgs& args);
  static constexpr void DebugAssertIsValid(const fuchsia_hardware_display::wire::Mode& fidl_mode);
  static constexpr void DebugAssertIsValid(const display_mode_t& banjo_mode);

  Dimensions active_area_;
  int32_t refresh_rate_millihertz_;
};

// static
constexpr bool Mode::IsValid(const fuchsia_hardware_display::wire::Mode& fidl_mode) {
  if (!Dimensions::IsValid((fidl_mode.active_area))) {
    return false;
  }

  const Dimensions active_area = Dimensions::From(fidl_mode.active_area);
  if (active_area.IsEmpty()) {
    return false;
  }

  if (fidl_mode.refresh_rate_millihertz <= 0) {
    return false;
  }
  if (fidl_mode.refresh_rate_millihertz > kMaxRefreshRateMillihertz) {
    return false;
  }

  if (fidl_mode.flags != fuchsia_hardware_display::wire::ModeFlags()) {
    return false;
  }

  return true;
}

// static
constexpr bool Mode::IsValid(const display_mode_t& banjo_mode) {
  const size_u_t banjo_active_area = {
      .width = banjo_mode.h_addressable,
      .height = banjo_mode.v_addressable,
  };
  if (!Dimensions::IsValid(banjo_active_area)) {
    return false;
  }

  const Dimensions active_area = Dimensions::From(banjo_active_area);
  ZX_DEBUG_ASSERT(!active_area.IsEmpty());

  if (banjo_mode.h_front_porch != 0) {
    return false;
  }
  if (banjo_mode.h_sync_pulse != 0) {
    return false;
  }
  if (banjo_mode.h_blanking != 0) {
    return false;
  }
  if (banjo_mode.v_front_porch != 0) {
    return false;
  }
  if (banjo_mode.v_sync_pulse != 0) {
    return false;
  }
  if (banjo_mode.v_blanking != 0) {
    return false;
  }

  if (banjo_mode.flags != 0) {
    return false;
  }

  if (banjo_mode.pixel_clock_hz <= 0) {
    return false;
  }

  // The active area is at most `Dimensions::kMaxWidth` x
  // `Dimensions::kMaxHeight`, so the number of pixels in the active area is at
  // most std::pow(2, 32).
  const int64_t active_area_pixels = int64_t{active_area.width()} * active_area.height();

  // The multiplication does not overflow (causing UB) because
  // `active_area_pixels` is at most std::pow(2, 32) and
  // `kMaxRefreshRateMillihertz` is less than std::pow(2, 20), so the product is
  // less than std::pow(2, 52).
  const int64_t max_pixel_clock_millihertz = active_area_pixels * kMaxRefreshRateMillihertz;

  if (banjo_mode.pixel_clock_hz > max_pixel_clock_millihertz / 1'000) {
    return false;
  }

  return true;
}

constexpr Mode::Mode(const Mode::ConstructorArgs& args)
    : active_area_({.width = args.active_width, .height = args.active_height}),
      refresh_rate_millihertz_(args.refresh_rate_millihertz) {
  DebugAssertIsValid(args);
}

// static
constexpr Mode Mode::From(const fuchsia_hardware_display::wire::Mode& fidl_mode) {
  DebugAssertIsValid(fidl_mode);
  const Dimensions active_area = Dimensions::From(fidl_mode.active_area);

  return Mode({
      .active_width = active_area.width(),
      .active_height = active_area.height(),
      .refresh_rate_millihertz = static_cast<int32_t>(fidl_mode.refresh_rate_millihertz),
  });
}

// static
constexpr Mode Mode::From(const display_mode_t& banjo_mode) {
  DebugAssertIsValid(banjo_mode);

  const Dimensions active_area = Dimensions::From(
      size_u_t{.width = banjo_mode.h_addressable, .height = banjo_mode.v_addressable});

  // The active area is at most `Dimensions::kMaxWidth` x
  // `Dimensions::kMaxHeight`, so the number of pixels in the active area is at
  // most std::pow(2, 32).
  const int64_t active_area_pixels = int64_t{active_area.width()} * active_area.height();

  // The multiplication does not overflow (causing UB) because the
  // display stack limitations checked in DebugAssertIsValid() guarantee that
  // the product is at most 2^52.
  const int64_t pixel_clock_millihertz = banjo_mode.pixel_clock_hz * 1'000;

  // The cast does not overflow (causing UB) because the display
  // stack limitations checked in DebugAssertIsValid() guarantee
  // that the rate is at most `kMaxRefreshRateMillihertz`.
  const int32_t refresh_rate_millihertz =
      static_cast<int32_t>(pixel_clock_millihertz / active_area_pixels);

  return Mode({
      .active_width = active_area.width(),
      .active_height = active_area.height(),
      .refresh_rate_millihertz = refresh_rate_millihertz,
  });
}

constexpr bool operator==(const Mode& lhs, const Mode& rhs) {
  return lhs.active_area_ == rhs.active_area_ &&
         lhs.refresh_rate_millihertz_ == rhs.refresh_rate_millihertz_;
}

constexpr bool operator!=(const Mode& lhs, const Mode& rhs) { return !(lhs == rhs); }

constexpr fuchsia_hardware_display::wire::Mode Mode::ToFidl() const {
  return fuchsia_hardware_display::wire::Mode{
      .active_area = active_area_.ToFidl(),

      // This cast does not cause UB because the refresh rate must be
      // non-negative.
      .refresh_rate_millihertz = static_cast<uint32_t>(refresh_rate_millihertz_),
  };
}

constexpr display_mode_t Mode::ToBanjo() const {
  const size_u_t banjo_active_area = active_area_.ToBanjo();

  // The active area is at most `Dimensions::kMaxWidth` x
  // `Dimensions::kMaxHeight`, so the number of pixels in the active area is at
  // most std::pow(2, 32).
  const int64_t active_area_pixels = int64_t{active_area_.width()} * active_area_.height();

  // The multiplication does not overflow (causing UB) because
  // `active_area_pixels` is at most std::pow(2, 32) and
  // `kMaxRefreshRateMillihertz` is less than std::pow(2, 20), so the product is
  // less than std::pow(2, 52).
  const int64_t pixel_clock_millihertz = active_area_pixels * refresh_rate_millihertz_;

  return display_mode_t{
      .pixel_clock_hz = pixel_clock_millihertz / 1'000,
      .h_addressable = banjo_active_area.width,
      .h_front_porch = 0,
      .h_sync_pulse = 0,
      .h_blanking = 0,
      .v_addressable = banjo_active_area.height,
      .v_front_porch = 0,
      .v_sync_pulse = 0,
      .v_blanking = 0,
      .flags = 0,
  };
}

// static
constexpr void Mode::DebugAssertIsValid(const Mode::ConstructorArgs& args) {
  const Dimensions active_area({.width = args.active_width, .height = args.active_height});
  ZX_DEBUG_ASSERT(!active_area.IsEmpty());

  ZX_DEBUG_ASSERT(args.refresh_rate_millihertz > 0);
  ZX_DEBUG_ASSERT(args.refresh_rate_millihertz <= kMaxRefreshRateMillihertz);
}

// static
constexpr void Mode::DebugAssertIsValid(const fuchsia_hardware_display::wire::Mode& fidl_mode) {
  ZX_DEBUG_ASSERT(Dimensions::IsValid(fidl_mode.active_area));
  const Dimensions active_area = Dimensions::From(fidl_mode.active_area);
  ZX_DEBUG_ASSERT(!active_area.IsEmpty());

  ZX_DEBUG_ASSERT(fidl_mode.refresh_rate_millihertz > 0);
  ZX_DEBUG_ASSERT(fidl_mode.refresh_rate_millihertz <= kMaxRefreshRateMillihertz);

  ZX_DEBUG_ASSERT(fidl_mode.flags == fuchsia_hardware_display::wire::ModeFlags());
}

// static
constexpr void Mode::DebugAssertIsValid(const display_mode_t& banjo_mode) {
  const size_u_t banjo_active_area = {
      .width = banjo_mode.h_addressable,
      .height = banjo_mode.v_addressable,
  };
  ZX_DEBUG_ASSERT(Dimensions::IsValid(banjo_active_area));
  const Dimensions active_area = Dimensions::From(banjo_active_area);
  ZX_DEBUG_ASSERT(!active_area.IsEmpty());

  ZX_DEBUG_ASSERT(banjo_mode.h_front_porch == 0);
  ZX_DEBUG_ASSERT(banjo_mode.h_sync_pulse == 0);
  ZX_DEBUG_ASSERT(banjo_mode.h_blanking == 0);
  ZX_DEBUG_ASSERT(banjo_mode.v_front_porch == 0);
  ZX_DEBUG_ASSERT(banjo_mode.v_sync_pulse == 0);
  ZX_DEBUG_ASSERT(banjo_mode.v_blanking == 0);

  ZX_DEBUG_ASSERT(banjo_mode.flags == 0);

  ZX_DEBUG_ASSERT(banjo_mode.pixel_clock_hz > 0);

  // The active area is at most `Dimensions::kMaxWidth` x
  // `Dimensions::kMaxHeight`, so the number of pixels in the active area is at
  // most std::pow(2, 32).
  const int64_t active_area_pixels = int64_t{active_area.width()} * active_area.height();

  // The multiplication does not overflow (causing UB) because
  // `active_area_pixels` is at most std::pow(2, 32) and
  // `kMaxRefreshRateMillihertz` is less than std::pow(2, 20), so the product is
  // less than std::pow(2, 52).
  const int64_t max_pixel_clock_millihertz = active_area_pixels * kMaxRefreshRateMillihertz;

  ZX_DEBUG_ASSERT_MSG(banjo_mode.pixel_clock_hz <= max_pixel_clock_millihertz,
                      "Pixel clock %" PRId64 " mHz exceeds the maximum %" PRId64 " mHz",
                      banjo_mode.pixel_clock_hz, max_pixel_clock_millihertz);
}

}  // namespace display

#endif  // SRC_GRAPHICS_DISPLAY_LIB_API_TYPES_CPP_MODE_H_
