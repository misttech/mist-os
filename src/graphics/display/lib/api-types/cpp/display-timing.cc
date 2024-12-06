// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types/cpp/display-timing.h"

#include <fidl/fuchsia.hardware.display.engine/cpp/wire.h>
#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <fuchsia/hardware/dsiimpl/c/banjo.h>
#include <zircon/assert.h>

#include <cstdint>

namespace display {

namespace {

constexpr uint32_t ToBanjoModeFlag(const DisplayTiming& display_timing_params) {
  uint32_t banjo_mode_flag = 0;
  if (display_timing_params.vsync_polarity == SyncPolarity::kPositive) {
    banjo_mode_flag |= MODE_FLAG_VSYNC_POSITIVE;
  }
  if (display_timing_params.hsync_polarity == SyncPolarity::kPositive) {
    banjo_mode_flag |= MODE_FLAG_HSYNC_POSITIVE;
  }
  if (display_timing_params.fields_per_frame == FieldsPerFrame::kInterlaced) {
    banjo_mode_flag |= MODE_FLAG_INTERLACED;
  }
  if (display_timing_params.vblank_alternates) {
    banjo_mode_flag |= MODE_FLAG_ALTERNATING_VBLANK;
  }
  ZX_DEBUG_ASSERT_MSG(
      display_timing_params.pixel_repetition == 0 || display_timing_params.pixel_repetition == 1,
      "Unsupported pixel_repetition: %d", display_timing_params.pixel_repetition);
  if (display_timing_params.pixel_repetition == 1) {
    banjo_mode_flag |= MODE_FLAG_DOUBLE_CLOCKED;
  }
  return banjo_mode_flag;
}

constexpr void DebugAssertBanjoDisplayModeIsValid(const display_mode_t& display_mode) {
  // The >= 0 assertions are always true for uint32_t members in the
  // `display_mode_t` struct and will be eventually optimized by the compiler.
  //
  // These assertions, depsite being always true, match the member
  // definitions in `DisplayTiming` and they make it easier for readers to
  // reason about the code without checking the types of each struct member.

  ZX_DEBUG_ASSERT(display_mode.pixel_clock_hz >= 0);
  ZX_DEBUG_ASSERT(display_mode.pixel_clock_hz <= kMaxPixelClockHz);

  ZX_DEBUG_ASSERT(display_mode.h_addressable >= 0);
  ZX_DEBUG_ASSERT(display_mode.h_addressable <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.h_front_porch >= 0);
  ZX_DEBUG_ASSERT(display_mode.h_front_porch <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.h_sync_pulse >= 0);
  ZX_DEBUG_ASSERT(display_mode.h_sync_pulse <= kMaxTimingValue);

  // `h_front_porch` and `h_sync_pulse` are both within [0..kMaxTimingValue],
  // so adding these two values won't cause an unsigned overflow.
  ZX_DEBUG_ASSERT(display_mode.h_blanking >=
                  display_mode.h_front_porch + display_mode.h_sync_pulse);
  ZX_DEBUG_ASSERT(display_mode.h_blanking -
                      (display_mode.h_front_porch + display_mode.h_sync_pulse) <=
                  kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.v_addressable >= 0);
  ZX_DEBUG_ASSERT(display_mode.v_addressable <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.v_front_porch >= 0);
  ZX_DEBUG_ASSERT(display_mode.v_front_porch <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.v_sync_pulse >= 0);
  ZX_DEBUG_ASSERT(display_mode.v_sync_pulse <= kMaxTimingValue);

  // `v_front_porch` and `v_sync_pulse` are both within [0..kMaxTimingValue],
  // so adding these two values won't cause an unsigned overflow.
  ZX_DEBUG_ASSERT(display_mode.v_blanking >=
                  display_mode.v_front_porch + display_mode.v_sync_pulse);
  ZX_DEBUG_ASSERT(display_mode.v_blanking -
                      (display_mode.v_front_porch + display_mode.v_sync_pulse) <=
                  kMaxTimingValue);

  constexpr uint32_t kFlagMask = MODE_FLAG_VSYNC_POSITIVE | MODE_FLAG_HSYNC_POSITIVE |
                                 MODE_FLAG_INTERLACED | MODE_FLAG_ALTERNATING_VBLANK |
                                 MODE_FLAG_DOUBLE_CLOCKED;
  ZX_DEBUG_ASSERT_MSG((display_mode.flags & (~kFlagMask)) == 0u,
                      "flags 0x%x has unknown bits: 0x%x", display_mode.flags,
                      display_mode.flags & (~kFlagMask));
}

constexpr void DebugAssertFidlDisplayModeIsValid(
    const fuchsia_hardware_display_engine::wire::DisplayMode& display_mode) {
  // The >= 0 assertions are always true for uint32_t members in the
  // `DisplayMode` struct and will be eventually optimized by the compiler.
  //
  // These assertions, depsite being always true, match the member
  // definitions in `DisplayTiming` and they make it easier for readers to
  // reason about the code without checking the types of each struct member.

  ZX_DEBUG_ASSERT(display_mode.pixel_clock_hz >= 0);
  ZX_DEBUG_ASSERT(display_mode.pixel_clock_hz <= kMaxPixelClockHz);

  ZX_DEBUG_ASSERT(display_mode.h_addressable >= 0);
  ZX_DEBUG_ASSERT(display_mode.h_addressable <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.h_front_porch >= 0);
  ZX_DEBUG_ASSERT(display_mode.h_front_porch <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.h_sync_pulse >= 0);
  ZX_DEBUG_ASSERT(display_mode.h_sync_pulse <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.h_blanking >= display_mode.h_front_porch);
  ZX_DEBUG_ASSERT(display_mode.h_blanking >= display_mode.h_sync_pulse);
  ZX_DEBUG_ASSERT(display_mode.h_blanking - display_mode.h_front_porch >=
                  display_mode.h_sync_pulse);
  ZX_DEBUG_ASSERT(display_mode.h_blanking - display_mode.h_front_porch -
                      display_mode.h_sync_pulse <=
                  kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.v_addressable >= 0);
  ZX_DEBUG_ASSERT(display_mode.v_addressable <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.v_front_porch >= 0);
  ZX_DEBUG_ASSERT(display_mode.v_front_porch <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.v_sync_pulse >= 0);
  ZX_DEBUG_ASSERT(display_mode.v_sync_pulse <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.v_blanking >= display_mode.v_front_porch);
  ZX_DEBUG_ASSERT(display_mode.v_blanking >= display_mode.v_sync_pulse);
  ZX_DEBUG_ASSERT(display_mode.v_blanking - display_mode.v_front_porch >=
                  display_mode.v_sync_pulse);
  ZX_DEBUG_ASSERT(display_mode.v_blanking - display_mode.v_front_porch -
                      display_mode.v_sync_pulse <=
                  kMaxTimingValue);
}

constexpr void DebugAssertBanjoDisplaySettingIsValid(const display_setting_t& display_setting) {
  // The >= 0 assertions are always true for uint32_t members in the
  // `display_setting_t` struct and will be eventually optimized by the
  // compiler.
  //
  // These assertions, despite being always true, match the member
  // definitions in `DisplayTiming` and they make it easier for readers to
  // reason about the code without checking the types of each struct member.

  ZX_DEBUG_ASSERT(display_setting.lcd_clock >= 0);
  ZX_DEBUG_ASSERT(int64_t{display_setting.lcd_clock} <= kMaxPixelClockHz);

  ZX_DEBUG_ASSERT(display_setting.h_active >= 0);
  ZX_DEBUG_ASSERT(display_setting.h_active <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.h_period >= 0);
  ZX_DEBUG_ASSERT(display_setting.h_active <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.hsync_bp >= 0);
  ZX_DEBUG_ASSERT(display_setting.hsync_bp <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.hsync_width >= 0);
  ZX_DEBUG_ASSERT(display_setting.hsync_width <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.hsync_pol == 0 || display_setting.hsync_pol == 1);

  // `h_active`, `hsync_bp` and `hsync_width` are all within
  // [0..kMaxTimingValue], so adding these values won't cause an unsigned
  // overflow.
  ZX_DEBUG_ASSERT(display_setting.h_period >= display_setting.h_active + display_setting.hsync_bp +
                                                  display_setting.hsync_width);
  ZX_DEBUG_ASSERT(display_setting.h_period - (display_setting.h_active + display_setting.hsync_bp +
                                              display_setting.hsync_width) <=
                  kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.v_active >= 0);
  ZX_DEBUG_ASSERT(display_setting.v_active <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.v_period >= 0);
  ZX_DEBUG_ASSERT(display_setting.v_active <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.vsync_bp >= 0);
  ZX_DEBUG_ASSERT(display_setting.vsync_bp <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.vsync_width >= 0);
  ZX_DEBUG_ASSERT(display_setting.vsync_width <= kMaxTimingValue);

  // `v_active`, `vsync_bp` and `vsync_width` are all within
  // [0..kMaxTimingValue], so adding these values won't cause an unsigned
  // overflow.
  ZX_DEBUG_ASSERT(display_setting.v_period >= display_setting.v_active + display_setting.vsync_bp +
                                                  display_setting.vsync_width);
  ZX_DEBUG_ASSERT(display_setting.v_period - (display_setting.v_active + display_setting.vsync_bp +
                                              display_setting.vsync_width) <=
                  kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_setting.vsync_pol == 0 || display_setting.vsync_pol == 1);
}

}  // namespace

DisplayTiming ToDisplayTiming(const display_mode_t& banjo_display_mode) {
  DebugAssertBanjoDisplayModeIsValid(banjo_display_mode);

  // A valid display_mode_t guarantees that both h_front_porch and h_sync_pulse
  // are no more than kMaxTimingValue, so (h_front_porch + h_addressable) won't
  // overflow.
  //
  // It also guarantees that h_blanking >= h_front_porch + h_sync_pulse, and
  // h_blanking - (h_front_porch + h_sync_pulse) won't overflow and will fit
  // in [0, kMaxTimingValue] -- so we can use int32_t.
  int32_t horizontal_back_porch_px =
      static_cast<int32_t>(banjo_display_mode.h_blanking -
                           (banjo_display_mode.h_front_porch + banjo_display_mode.h_sync_pulse));

  // Ditto for the vertical back porch.
  int32_t vertical_back_porch_lines =
      static_cast<int32_t>(banjo_display_mode.v_blanking -
                           (banjo_display_mode.v_front_porch + banjo_display_mode.v_sync_pulse));

  return DisplayTiming{
      .horizontal_active_px = static_cast<int32_t>(banjo_display_mode.h_addressable),
      .horizontal_front_porch_px = static_cast<int32_t>(banjo_display_mode.h_front_porch),
      .horizontal_sync_width_px = static_cast<int32_t>(banjo_display_mode.h_sync_pulse),
      .horizontal_back_porch_px = horizontal_back_porch_px,
      .vertical_active_lines = static_cast<int32_t>(banjo_display_mode.v_addressable),
      .vertical_front_porch_lines = static_cast<int32_t>(banjo_display_mode.v_front_porch),
      .vertical_sync_width_lines = static_cast<int32_t>(banjo_display_mode.v_sync_pulse),
      .vertical_back_porch_lines = vertical_back_porch_lines,
      .pixel_clock_frequency_hz = banjo_display_mode.pixel_clock_hz,
      .fields_per_frame = (banjo_display_mode.flags & MODE_FLAG_INTERLACED)
                              ? FieldsPerFrame::kInterlaced
                              : FieldsPerFrame::kProgressive,
      .hsync_polarity = (banjo_display_mode.flags & MODE_FLAG_HSYNC_POSITIVE)
                            ? SyncPolarity::kPositive
                            : SyncPolarity::kNegative,
      .vsync_polarity = (banjo_display_mode.flags & MODE_FLAG_VSYNC_POSITIVE)
                            ? SyncPolarity::kPositive
                            : SyncPolarity::kNegative,
      .vblank_alternates = (banjo_display_mode.flags & MODE_FLAG_ALTERNATING_VBLANK) != 0,
      .pixel_repetition = (banjo_display_mode.flags & MODE_FLAG_DOUBLE_CLOCKED) ? 1 : 0,
  };
}

DisplayTiming ToDisplayTiming(
    const fuchsia_hardware_display_engine::wire::DisplayMode& fidl_display_mode) {
  DebugAssertFidlDisplayModeIsValid(fidl_display_mode);

  // A valid DisplayMode guarantees that both h_front_porch and h_sync_pulse
  // are no more than kMaxTimingValue, so (h_front_porch + h_addressable) won't
  // overflow.
  //
  // It also guarantees that h_blanking >= h_front_porch + h_sync_pulse, and
  // h_blanking - (h_front_porch + h_sync_pulse) won't overflow and will fit
  // in [0, kMaxTimingValue] -- so we can use int32_t.
  int32_t horizontal_back_porch_px =
      static_cast<int32_t>(fidl_display_mode.h_blanking -
                           (fidl_display_mode.h_front_porch + fidl_display_mode.h_sync_pulse));

  // Ditto for the vertical back porch.
  int32_t vertical_back_porch_lines =
      static_cast<int32_t>(fidl_display_mode.v_blanking -
                           (fidl_display_mode.v_front_porch + fidl_display_mode.v_sync_pulse));

  return DisplayTiming{
      .horizontal_active_px = static_cast<int32_t>(fidl_display_mode.h_addressable),
      .horizontal_front_porch_px = static_cast<int32_t>(fidl_display_mode.h_front_porch),
      .horizontal_sync_width_px = static_cast<int32_t>(fidl_display_mode.h_sync_pulse),
      .horizontal_back_porch_px = horizontal_back_porch_px,
      .vertical_active_lines = static_cast<int32_t>(fidl_display_mode.v_addressable),
      .vertical_front_porch_lines = static_cast<int32_t>(fidl_display_mode.v_front_porch),
      .vertical_sync_width_lines = static_cast<int32_t>(fidl_display_mode.v_sync_pulse),
      .vertical_back_porch_lines = vertical_back_porch_lines,
      .pixel_clock_frequency_hz = fidl_display_mode.pixel_clock_hz,
      .fields_per_frame =
          (fidl_display_mode.flags & fuchsia_hardware_display_engine::wire::ModeFlag::kInterlaced)
              ? FieldsPerFrame::kInterlaced
              : FieldsPerFrame::kProgressive,
      .hsync_polarity = (fidl_display_mode.flags &
                         fuchsia_hardware_display_engine::wire::ModeFlag::kHsyncPositive)
                            ? SyncPolarity::kPositive
                            : SyncPolarity::kNegative,
      .vsync_polarity = (fidl_display_mode.flags &
                         fuchsia_hardware_display_engine::wire::ModeFlag::kVsyncPositive)
                            ? SyncPolarity::kPositive
                            : SyncPolarity::kNegative,
      .vblank_alternates =
          static_cast<bool>(fidl_display_mode.flags &
                            fuchsia_hardware_display_engine::wire::ModeFlag::kAlternatingVblank),
      .pixel_repetition = (fidl_display_mode.flags &
                           fuchsia_hardware_display_engine::wire::ModeFlag::kDoubleClocked)
                              ? 1
                              : 0,
  };
}

display_mode_t ToBanjoDisplayMode(const DisplayTiming& display_timing_params) {
  display_timing_params.DebugAssertIsValid();
  return display_mode_t{
      .pixel_clock_hz = display_timing_params.pixel_clock_frequency_hz,
      .h_addressable = static_cast<uint32_t>(display_timing_params.horizontal_active_px),
      .h_front_porch = static_cast<uint32_t>(display_timing_params.horizontal_front_porch_px),
      .h_sync_pulse = static_cast<uint32_t>(display_timing_params.horizontal_sync_width_px),
      // Hfront, hsync and hback are all within [0, kMaxTimingValue], so the
      // sum is also a valid 32-bit unsigned integer.
      .h_blanking = static_cast<uint32_t>(display_timing_params.horizontal_front_porch_px +
                                          display_timing_params.horizontal_sync_width_px +
                                          display_timing_params.horizontal_back_porch_px),
      .v_addressable = static_cast<uint32_t>(display_timing_params.vertical_active_lines),
      .v_front_porch = static_cast<uint32_t>(display_timing_params.vertical_front_porch_lines),
      .v_sync_pulse = static_cast<uint32_t>(display_timing_params.vertical_sync_width_lines),
      // Vfront, vsync and vback are all within [0, kMaxTimingValue], so the
      // sum is also a valid 32-bit unsigned integer.
      .v_blanking = static_cast<uint32_t>(display_timing_params.vertical_front_porch_lines +
                                          display_timing_params.vertical_sync_width_lines +
                                          display_timing_params.vertical_back_porch_lines),
      .flags = ToBanjoModeFlag(display_timing_params),
  };
}

DisplayTiming ToDisplayTiming(const display_setting_t& banjo_display_setting) {
  DebugAssertBanjoDisplaySettingIsValid(banjo_display_setting);

  // A valid display_setting_t guarantees that `h_active`, `hsync_bp` and
  // `hsync_width` are all within [0..kMaxTimingValue],  so (h_active +
  // hsync_bp + hsync_width) won't overflow.
  //
  // It also guarantees that h_period >= (h_active + hsync_bp + hsync_width),
  // and h_period - (h_active + hsync_bp + hsync_width) is within
  // [0, kMaxTimingValue], so we can use int32_t to store its value.
  const int32_t horizontal_front_porch_px =
      static_cast<int32_t>(banjo_display_setting.h_period -
                           (banjo_display_setting.h_active + banjo_display_setting.hsync_bp +
                            banjo_display_setting.hsync_width));

  // Using an argument similar to the above, we can prove that the vertical
  // front porch value can be also stored in an int32_t.
  const int32_t vertical_front_porch_lines =
      static_cast<int32_t>(banjo_display_setting.v_period -
                           (banjo_display_setting.v_active + banjo_display_setting.vsync_bp +
                            banjo_display_setting.vsync_width));

  return DisplayTiming{
      .horizontal_active_px = static_cast<int32_t>(banjo_display_setting.h_active),
      .horizontal_front_porch_px = static_cast<int32_t>(horizontal_front_porch_px),
      .horizontal_sync_width_px = static_cast<int32_t>(banjo_display_setting.hsync_width),
      .horizontal_back_porch_px = static_cast<int32_t>(banjo_display_setting.hsync_bp),
      .vertical_active_lines = static_cast<int32_t>(banjo_display_setting.v_active),
      .vertical_front_porch_lines = vertical_front_porch_lines,
      .vertical_sync_width_lines = static_cast<int32_t>(banjo_display_setting.vsync_width),
      .vertical_back_porch_lines = static_cast<int32_t>(banjo_display_setting.vsync_bp),
      .pixel_clock_frequency_hz = banjo_display_setting.lcd_clock,
      .fields_per_frame = FieldsPerFrame::kProgressive,
      .hsync_polarity = (banjo_display_setting.hsync_pol == 1) ? SyncPolarity::kPositive
                                                               : SyncPolarity::kNegative,
      .vsync_polarity = (banjo_display_setting.vsync_pol == 1) ? SyncPolarity::kPositive
                                                               : SyncPolarity::kNegative,
      .vblank_alternates = false,
      .pixel_repetition = 0,
  };
}

}  // namespace display
