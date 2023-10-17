// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/api-types-cpp/display-timing.h"

#include <fuchsia/hardware/display/controller/c/banjo.h>
#include <zircon/assert.h>

#include <limits>

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

constexpr fuchsia_hardware_hdmi::wire::ModeFlag ToHdmiFidlModeFlag(
    const DisplayTiming& display_timing_params) {
  fuchsia_hardware_hdmi::wire::ModeFlag fidl_mode_flag(0);
  if (display_timing_params.vsync_polarity == SyncPolarity::kPositive) {
    fidl_mode_flag |= fuchsia_hardware_hdmi::wire::ModeFlag::kVsyncPositive;
  }
  if (display_timing_params.hsync_polarity == SyncPolarity::kPositive) {
    fidl_mode_flag |= fuchsia_hardware_hdmi::wire::ModeFlag::kHsyncPositive;
  }
  if (display_timing_params.fields_per_frame == FieldsPerFrame::kInterlaced) {
    fidl_mode_flag |= fuchsia_hardware_hdmi::wire::ModeFlag::kInterlaced;
  }
  if (display_timing_params.vblank_alternates) {
    fidl_mode_flag |= fuchsia_hardware_hdmi::wire::ModeFlag::kAlternatingVblank;
  }
  ZX_DEBUG_ASSERT_MSG(
      display_timing_params.pixel_repetition == 0 || display_timing_params.pixel_repetition == 1,
      "Unsupported pixel_repetition: %d", display_timing_params.pixel_repetition);
  if (display_timing_params.pixel_repetition == 1) {
    fidl_mode_flag |= fuchsia_hardware_hdmi::wire::ModeFlag::kDoubleClocked;
  }
  return fidl_mode_flag;
}

constexpr void DebugAssertDisplayTimingIsValid(const DisplayTiming& timing_params) {
  ZX_DEBUG_ASSERT(timing_params.horizontal_active_px >= 0);
  ZX_DEBUG_ASSERT(timing_params.horizontal_active_px <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(timing_params.horizontal_front_porch_px >= 0);
  ZX_DEBUG_ASSERT(timing_params.horizontal_front_porch_px <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(timing_params.horizontal_sync_width_px >= 0);
  ZX_DEBUG_ASSERT(timing_params.horizontal_sync_width_px <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(timing_params.horizontal_blank_px >= 0);
  ZX_DEBUG_ASSERT(timing_params.horizontal_blank_px <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(timing_params.vertical_active_lines >= 0);
  ZX_DEBUG_ASSERT(timing_params.vertical_active_lines <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(timing_params.vertical_front_porch_lines >= 0);
  ZX_DEBUG_ASSERT(timing_params.vertical_front_porch_lines <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(timing_params.vertical_sync_width_lines >= 0);
  ZX_DEBUG_ASSERT(timing_params.vertical_sync_width_lines <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(timing_params.vertical_blank_lines >= 0);
  ZX_DEBUG_ASSERT(timing_params.vertical_blank_lines <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(timing_params.pixel_clock_frequency_khz >= 0);
  ZX_DEBUG_ASSERT(timing_params.pixel_clock_frequency_khz <= kMaxPixelClockKhz);

  ZX_DEBUG_ASSERT(timing_params.pixel_repetition >= 0);
  ZX_DEBUG_ASSERT(timing_params.pixel_repetition <= 9);
}

constexpr void DebugAssertBanjoDisplayModeIsValid(const display_mode_t& display_mode) {
  // The >= 0 assertions are always true for uint32_t members in the
  // `display_mode_t` struct and will be eventually optimized by the compiler.
  //
  // These assertions, depsite being always true, match the member
  // definitions in `DisplayTiming` and they make it easier for readers to
  // reason about the code without checking the types of each struct member.

  ZX_DEBUG_ASSERT(display_mode.pixel_clock_khz >= 0);
  ZX_DEBUG_ASSERT(display_mode.pixel_clock_khz <= kMaxPixelClockKhz);

  ZX_DEBUG_ASSERT(display_mode.h_addressable >= 0);
  ZX_DEBUG_ASSERT(display_mode.h_addressable <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.h_front_porch >= 0);
  ZX_DEBUG_ASSERT(display_mode.h_front_porch <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.h_sync_pulse >= 0);
  ZX_DEBUG_ASSERT(display_mode.h_sync_pulse <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.h_blanking >= 0);
  ZX_DEBUG_ASSERT(display_mode.h_blanking <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.v_addressable >= 0);
  ZX_DEBUG_ASSERT(display_mode.v_addressable <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.v_front_porch >= 0);
  ZX_DEBUG_ASSERT(display_mode.v_front_porch <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.v_sync_pulse >= 0);
  ZX_DEBUG_ASSERT(display_mode.v_sync_pulse <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.v_blanking >= 0);
  ZX_DEBUG_ASSERT(display_mode.v_blanking <= kMaxTimingValue);

  constexpr uint32_t kFlagMask = MODE_FLAG_VSYNC_POSITIVE | MODE_FLAG_HSYNC_POSITIVE |
                                 MODE_FLAG_INTERLACED | MODE_FLAG_ALTERNATING_VBLANK |
                                 MODE_FLAG_DOUBLE_CLOCKED;
  ZX_DEBUG_ASSERT_MSG((display_mode.flags & (~kFlagMask)) == 0u,
                      "flags 0x%x has unknown bits: 0x%x", display_mode.flags,
                      display_mode.flags & (~kFlagMask));
}

constexpr void DebugAssertHdmiFidlDisplayModeIsValid(
    const fuchsia_hardware_hdmi::wire::StandardDisplayMode& display_mode) {
  // The >= 0 assertions are always true for uint32_t members in the
  // `StandardDisplayMode` struct and will be eventually optimized by the
  // compiler.
  //
  // These assertions, depsite being always true, match the member
  // definitions in `DisplayTiming` and they make it easier for readers to
  // reason about the code without checking the types of each struct member.

  ZX_DEBUG_ASSERT(display_mode.pixel_clock_khz >= 0);
  ZX_DEBUG_ASSERT(display_mode.pixel_clock_khz <= kMaxPixelClockKhz);

  ZX_DEBUG_ASSERT(display_mode.h_addressable >= 0);
  ZX_DEBUG_ASSERT(display_mode.h_addressable <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.h_front_porch >= 0);
  ZX_DEBUG_ASSERT(display_mode.h_front_porch <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.h_sync_pulse >= 0);
  ZX_DEBUG_ASSERT(display_mode.h_sync_pulse <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.h_blanking >= 0);
  ZX_DEBUG_ASSERT(display_mode.h_blanking <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.v_addressable >= 0);
  ZX_DEBUG_ASSERT(display_mode.v_addressable <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.v_front_porch >= 0);
  ZX_DEBUG_ASSERT(display_mode.v_front_porch <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.v_sync_pulse >= 0);
  ZX_DEBUG_ASSERT(display_mode.v_sync_pulse <= kMaxTimingValue);

  ZX_DEBUG_ASSERT(display_mode.v_blanking >= 0);
  ZX_DEBUG_ASSERT(display_mode.v_blanking <= kMaxTimingValue);

  fuchsia_hardware_hdmi::wire::ModeFlag mode_flag(display_mode.flags);
  ZX_DEBUG_ASSERT_MSG(!mode_flag.has_unknown_bits(), "flags 0x%x has unknown bits: 0x%x",
                      display_mode.flags, static_cast<uint32_t>(mode_flag.unknown_bits()));
}

}  // namespace

DisplayTiming ToDisplayTiming(const display_mode_t& banjo_display_mode) {
  DebugAssertBanjoDisplayModeIsValid(banjo_display_mode);
  return DisplayTiming{
      .horizontal_active_px = static_cast<int32_t>(banjo_display_mode.h_addressable),
      .horizontal_front_porch_px = static_cast<int32_t>(banjo_display_mode.h_front_porch),
      .horizontal_sync_width_px = static_cast<int32_t>(banjo_display_mode.h_sync_pulse),
      .horizontal_blank_px = static_cast<int32_t>(banjo_display_mode.h_blanking),
      .vertical_active_lines = static_cast<int32_t>(banjo_display_mode.v_addressable),
      .vertical_front_porch_lines = static_cast<int32_t>(banjo_display_mode.v_front_porch),
      .vertical_sync_width_lines = static_cast<int32_t>(banjo_display_mode.v_sync_pulse),
      .vertical_blank_lines = static_cast<int32_t>(banjo_display_mode.v_blanking),
      .pixel_clock_frequency_khz = static_cast<int32_t>(banjo_display_mode.pixel_clock_khz),
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
    const fuchsia_hardware_hdmi::wire::StandardDisplayMode& fidl_display_mode) {
  DebugAssertHdmiFidlDisplayModeIsValid(fidl_display_mode);
  fuchsia_hardware_hdmi::wire::ModeFlag fidl_flags(fidl_display_mode.flags);
  return DisplayTiming{
      .horizontal_active_px = static_cast<int32_t>(fidl_display_mode.h_addressable),
      .horizontal_front_porch_px = static_cast<int32_t>(fidl_display_mode.h_front_porch),
      .horizontal_sync_width_px = static_cast<int32_t>(fidl_display_mode.h_sync_pulse),
      .horizontal_blank_px = static_cast<int32_t>(fidl_display_mode.h_blanking),
      .vertical_active_lines = static_cast<int32_t>(fidl_display_mode.v_addressable),
      .vertical_front_porch_lines = static_cast<int32_t>(fidl_display_mode.v_front_porch),
      .vertical_sync_width_lines = static_cast<int32_t>(fidl_display_mode.v_sync_pulse),
      .vertical_blank_lines = static_cast<int32_t>(fidl_display_mode.v_blanking),
      .pixel_clock_frequency_khz = static_cast<int32_t>(fidl_display_mode.pixel_clock_khz),
      .fields_per_frame = (fidl_flags & fuchsia_hardware_hdmi::wire::ModeFlag::kInterlaced)
                              ? FieldsPerFrame::kInterlaced
                              : FieldsPerFrame::kProgressive,
      .hsync_polarity = (fidl_flags & fuchsia_hardware_hdmi::wire::ModeFlag::kHsyncPositive)
                            ? SyncPolarity::kPositive
                            : SyncPolarity::kNegative,
      .vsync_polarity = (fidl_flags & fuchsia_hardware_hdmi::wire::ModeFlag::kVsyncPositive)
                            ? SyncPolarity::kPositive
                            : SyncPolarity::kNegative,
      .vblank_alternates =
          static_cast<bool>(fidl_flags & fuchsia_hardware_hdmi::wire::ModeFlag::kAlternatingVblank),
      .pixel_repetition =
          (fidl_flags & fuchsia_hardware_hdmi::wire::ModeFlag::kDoubleClocked) ? 1 : 0,
  };
}

display_mode_t ToBanjoDisplayMode(const DisplayTiming& display_timing_params) {
  DebugAssertDisplayTimingIsValid(display_timing_params);
  return display_mode_t{
      .pixel_clock_khz = static_cast<uint32_t>(display_timing_params.pixel_clock_frequency_khz),
      .h_addressable = static_cast<uint32_t>(display_timing_params.horizontal_active_px),
      .h_front_porch = static_cast<uint32_t>(display_timing_params.horizontal_front_porch_px),
      .h_sync_pulse = static_cast<uint32_t>(display_timing_params.horizontal_sync_width_px),
      .h_blanking = static_cast<uint32_t>(display_timing_params.horizontal_blank_px),
      .v_addressable = static_cast<uint32_t>(display_timing_params.vertical_active_lines),
      .v_front_porch = static_cast<uint32_t>(display_timing_params.vertical_front_porch_lines),
      .v_sync_pulse = static_cast<uint32_t>(display_timing_params.vertical_sync_width_lines),
      .v_blanking = static_cast<uint32_t>(display_timing_params.vertical_blank_lines),
      .flags = ToBanjoModeFlag(display_timing_params),
  };
}

fuchsia_hardware_hdmi::wire::StandardDisplayMode ToHdmiFidlStandardDisplayMode(
    const DisplayTiming& display_timing_params) {
  DebugAssertDisplayTimingIsValid(display_timing_params);
  return fuchsia_hardware_hdmi::wire::StandardDisplayMode{
      .pixel_clock_khz = static_cast<uint32_t>(display_timing_params.pixel_clock_frequency_khz),
      .h_addressable = static_cast<uint32_t>(display_timing_params.horizontal_active_px),
      .h_front_porch = static_cast<uint32_t>(display_timing_params.horizontal_front_porch_px),
      .h_sync_pulse = static_cast<uint32_t>(display_timing_params.horizontal_sync_width_px),
      .h_blanking = static_cast<uint32_t>(display_timing_params.horizontal_blank_px),
      .v_addressable = static_cast<uint32_t>(display_timing_params.vertical_active_lines),
      .v_front_porch = static_cast<uint32_t>(display_timing_params.vertical_front_porch_lines),
      .v_sync_pulse = static_cast<uint32_t>(display_timing_params.vertical_sync_width_lines),
      .v_blanking = static_cast<uint32_t>(display_timing_params.vertical_blank_lines),
      .flags = static_cast<uint32_t>(ToHdmiFidlModeFlag(display_timing_params)),
  };
}

}  // namespace display
