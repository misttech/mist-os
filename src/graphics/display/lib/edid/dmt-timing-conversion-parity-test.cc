// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/graphics/display/lib/edid/dmt-timing.h"
#include "src/graphics/display/lib/edid/timings.h"

namespace edid::internal {

namespace {

const DmtTiming& FindDmtTimingById(int32_t id) {
  const auto dmt_timing_it = std::find_if(kDmtTimings.begin(), kDmtTimings.end(),
                                          [id](const DmtTiming& dmt) { return dmt.id == id; });
  ZX_ASSERT(dmt_timing_it != kDmtTimings.end());
  return *dmt_timing_it;
}

// TODO(liyl): The following `ConvertedDmtTimingMatchesTimingParams` tests are
// only used as one-offs in order to make sure that there's no binary change to
// the `timing_params_t` array for all DMT timings. We can delete them once the
// code is merged.
TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x01) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x01);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 3150,
      .horizontal_addressable = 640,
      .horizontal_front_porch = 32,
      .horizontal_sync_pulse = 64,
      .horizontal_blanking = 192,
      .vertical_addressable = 350,
      .vertical_front_porch = 32,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 95,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 8508,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x02) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x02);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 3150,
      .horizontal_addressable = 640,
      .horizontal_front_porch = 32,
      .horizontal_sync_pulse = 64,
      .horizontal_blanking = 192,
      .vertical_addressable = 400,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 8508,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x03) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x03);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 3550,
      .horizontal_addressable = 720,
      .horizontal_front_porch = 36,
      .horizontal_sync_pulse = 72,
      .horizontal_blanking = 216,
      .vertical_addressable = 400,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 46,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 8504,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x04) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x04);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 2518,
      .horizontal_addressable = 640,
      .horizontal_front_porch = 16,
      .horizontal_sync_pulse = 96,
      .horizontal_blanking = 160,
      .vertical_addressable = 480,
      .vertical_front_porch = 10,
      .vertical_sync_pulse = 2,
      .vertical_blanking = 45,
      .flags = 0,
      .vertical_refresh_e2 = 5994,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x05) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x05);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 3150,
      .horizontal_addressable = 640,
      .horizontal_front_porch = 24,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 192,
      .vertical_addressable = 480,
      .vertical_front_porch = 9,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 40,
      .flags = 0,
      .vertical_refresh_e2 = 7281,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x06) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x06);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 3150,
      .horizontal_addressable = 640,
      .horizontal_front_porch = 16,
      .horizontal_sync_pulse = 64,
      .horizontal_blanking = 200,
      .vertical_addressable = 480,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 20,
      .flags = 0,
      .vertical_refresh_e2 = 7500,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x07) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x07);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 3600,
      .horizontal_addressable = 640,
      .horizontal_front_porch = 56,
      .horizontal_sync_pulse = 56,
      .horizontal_blanking = 192,
      .vertical_addressable = 480,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 29,
      .flags = 0,
      .vertical_refresh_e2 = 8501,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x08) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x08);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 3600,
      .horizontal_addressable = 800,
      .horizontal_front_porch = 24,
      .horizontal_sync_pulse = 72,
      .horizontal_blanking = 224,
      .vertical_addressable = 600,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 2,
      .vertical_blanking = 25,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 5625,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x09) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x09);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 4000,
      .horizontal_addressable = 800,
      .horizontal_front_porch = 40,
      .horizontal_sync_pulse = 128,
      .horizontal_blanking = 256,
      .vertical_addressable = 600,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 4,
      .vertical_blanking = 28,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 6032,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x0A) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x0A);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5000,
      .horizontal_addressable = 800,
      .horizontal_front_porch = 56,
      .horizontal_sync_pulse = 120,
      .horizontal_blanking = 240,
      .vertical_addressable = 600,
      .vertical_front_porch = 37,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 66,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 7219,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x0B) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x0B);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 4950,
      .horizontal_addressable = 800,
      .horizontal_front_porch = 16,
      .horizontal_sync_pulse = 80,
      .horizontal_blanking = 256,
      .vertical_addressable = 600,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 25,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 7500,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x0C) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x0C);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5625,
      .horizontal_addressable = 800,
      .horizontal_front_porch = 32,
      .horizontal_sync_pulse = 64,
      .horizontal_blanking = 248,
      .vertical_addressable = 600,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 31,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 8506,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x0D) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x0D);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7325,
      .horizontal_addressable = 800,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 600,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 4,
      .vertical_blanking = 36,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 11997,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x0E) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x0E);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 3375,
      .horizontal_addressable = 848,
      .horizontal_front_porch = 16,
      .horizontal_sync_pulse = 112,
      .horizontal_blanking = 240,
      .vertical_addressable = 480,
      .vertical_front_porch = 6,
      .vertical_sync_pulse = 8,
      .vertical_blanking = 37,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 6000,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x0F) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x0F);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 4490,
      .horizontal_addressable = 1024,
      .horizontal_front_porch = 8,
      .horizontal_sync_pulse = 176,
      .horizontal_blanking = 240,
      .vertical_addressable = 768,
      .vertical_front_porch = 0,
      .vertical_sync_pulse = 4,
      .vertical_blanking = 24,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync |
               timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank,
      .vertical_refresh_e2 = 8696,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x10) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x10);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 6500,
      .horizontal_addressable = 1024,
      .horizontal_front_porch = 24,
      .horizontal_sync_pulse = 136,
      .horizontal_blanking = 320,
      .vertical_addressable = 768,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 38,
      .flags = 0,
      .vertical_refresh_e2 = 6000,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x11) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x11);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7500,
      .horizontal_addressable = 1024,
      .horizontal_front_porch = 24,
      .horizontal_sync_pulse = 136,
      .horizontal_blanking = 304,
      .vertical_addressable = 768,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 38,
      .flags = 0,
      .vertical_refresh_e2 = 7007,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x12) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x12);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7875,
      .horizontal_addressable = 1024,
      .horizontal_front_porch = 16,
      .horizontal_sync_pulse = 96,
      .horizontal_blanking = 288,
      .vertical_addressable = 768,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 32,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 7503,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x13) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x13);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 9450,
      .horizontal_addressable = 1024,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 96,
      .horizontal_blanking = 352,
      .vertical_addressable = 768,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 40,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 8500,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x14) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x14);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 11550,
      .horizontal_addressable = 1024,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 768,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 4,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 11999,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x15) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x15);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10800,
      .horizontal_addressable = 1152,
      .horizontal_front_porch = 64,
      .horizontal_sync_pulse = 128,
      .horizontal_blanking = 448,
      .vertical_addressable = 864,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 36,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 7500,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x16) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x16);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 6825,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 768,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 7,
      .vertical_blanking = 22,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 6000,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x17) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x17);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7950,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 64,
      .horizontal_sync_pulse = 128,
      .horizontal_blanking = 384,
      .vertical_addressable = 768,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 7,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 5987,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x18) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x18);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10225,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 80,
      .horizontal_sync_pulse = 128,
      .horizontal_blanking = 416,
      .vertical_addressable = 768,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 7,
      .vertical_blanking = 37,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 7489,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x19) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x19);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 11750,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 80,
      .horizontal_sync_pulse = 136,
      .horizontal_blanking = 432,
      .vertical_addressable = 768,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 7,
      .vertical_blanking = 41,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 8484,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x1A) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x1A);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 14025,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 768,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 7,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 11980,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x1B) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x1B);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7100,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 800,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 23,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 5991,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x1C) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x1C);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 8350,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 72,
      .horizontal_sync_pulse = 128,
      .horizontal_blanking = 400,
      .vertical_addressable = 800,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 31,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 5981,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x1D) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x1D);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10650,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 80,
      .horizontal_sync_pulse = 128,
      .horizontal_blanking = 416,
      .vertical_addressable = 800,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 38,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 7493,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x1E) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x1E);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 12250,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 80,
      .horizontal_sync_pulse = 136,
      .horizontal_blanking = 432,
      .vertical_addressable = 800,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 43,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 8488,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x1F) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x1F);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 14625,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 800,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 47,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 11991,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x20) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x20);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10800,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 96,
      .horizontal_sync_pulse = 112,
      .horizontal_blanking = 520,
      .vertical_addressable = 960,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 40,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 6000,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x21) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x21);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 14850,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 64,
      .horizontal_sync_pulse = 160,
      .horizontal_blanking = 448,
      .vertical_addressable = 960,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 51,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 8500,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x22) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x22);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 17550,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 960,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 4,
      .vertical_blanking = 57,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 11984,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x23) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x23);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10800,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 112,
      .horizontal_blanking = 408,
      .vertical_addressable = 1024,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 42,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 6002,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x24) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x24);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 13500,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 16,
      .horizontal_sync_pulse = 144,
      .horizontal_blanking = 408,
      .vertical_addressable = 1024,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 42,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 7503,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x25) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x25);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 15750,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 64,
      .horizontal_sync_pulse = 160,
      .horizontal_blanking = 448,
      .vertical_addressable = 1024,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 48,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 8502,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x26) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x26);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 18725,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 1024,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 7,
      .vertical_blanking = 60,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 11996,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x27) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x27);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 8550,
      .horizontal_addressable = 1360,
      .horizontal_front_porch = 64,
      .horizontal_sync_pulse = 112,
      .horizontal_blanking = 432,
      .vertical_addressable = 768,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 27,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 6002,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x28) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x28);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 14825,
      .horizontal_addressable = 1360,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 768,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 11997,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x29) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x29);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10100,
      .horizontal_addressable = 1400,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 1050,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 4,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 5995,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x2A) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x2A);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 12175,
      .horizontal_addressable = 1400,
      .horizontal_front_porch = 88,
      .horizontal_sync_pulse = 144,
      .horizontal_blanking = 464,
      .vertical_addressable = 1050,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 4,
      .vertical_blanking = 39,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 5998,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x2B) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x2B);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 15600,
      .horizontal_addressable = 1400,
      .horizontal_front_porch = 104,
      .horizontal_sync_pulse = 144,
      .horizontal_blanking = 496,
      .vertical_addressable = 1050,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 4,
      .vertical_blanking = 49,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 7487,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x2C) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x2C);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 17950,
      .horizontal_addressable = 1400,
      .horizontal_front_porch = 104,
      .horizontal_sync_pulse = 152,
      .horizontal_blanking = 512,
      .vertical_addressable = 1050,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 4,
      .vertical_blanking = 55,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 8496,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x2D) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x2D);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 20800,
      .horizontal_addressable = 1400,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 1050,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 4,
      .vertical_blanking = 62,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 11990,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x2E) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x2E);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 8875,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 900,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 26,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 5990,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x2F) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x2F);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10650,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 80,
      .horizontal_sync_pulse = 152,
      .horizontal_blanking = 464,
      .vertical_addressable = 900,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 34,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 5989,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x30) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x30);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 13675,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 96,
      .horizontal_sync_pulse = 152,
      .horizontal_blanking = 496,
      .vertical_addressable = 900,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 42,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 7498,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x31) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x31);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 15700,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 104,
      .horizontal_sync_pulse = 152,
      .horizontal_blanking = 512,
      .vertical_addressable = 900,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 48,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 8484,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x32) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x32);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 18275,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 900,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 53,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 11985,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x33) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x33);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 16200,
      .horizontal_addressable = 1600,
      .horizontal_front_porch = 64,
      .horizontal_sync_pulse = 192,
      .horizontal_blanking = 560,
      .vertical_addressable = 1200,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 50,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 6000,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x34) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x34);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 17550,
      .horizontal_addressable = 1600,
      .horizontal_front_porch = 64,
      .horizontal_sync_pulse = 192,
      .horizontal_blanking = 560,
      .vertical_addressable = 1200,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 50,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 6500,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x35) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x35);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 18900,
      .horizontal_addressable = 1600,
      .horizontal_front_porch = 64,
      .horizontal_sync_pulse = 192,
      .horizontal_blanking = 560,
      .vertical_addressable = 1200,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 50,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 7000,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x36) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x36);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 20250,
      .horizontal_addressable = 1600,
      .horizontal_front_porch = 64,
      .horizontal_sync_pulse = 192,
      .horizontal_blanking = 560,
      .vertical_addressable = 1200,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 50,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 7500,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x37) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x37);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 22950,
      .horizontal_addressable = 1600,
      .horizontal_front_porch = 64,
      .horizontal_sync_pulse = 192,
      .horizontal_blanking = 560,
      .vertical_addressable = 1200,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 50,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 8500,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x38) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x38);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 26825,
      .horizontal_addressable = 1600,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 1200,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 4,
      .vertical_blanking = 71,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 11992,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x39) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x39);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 11900,
      .horizontal_addressable = 1680,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 1050,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 5988,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x3A) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x3A);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 14625,
      .horizontal_addressable = 1680,
      .horizontal_front_porch = 104,
      .horizontal_sync_pulse = 176,
      .horizontal_blanking = 560,
      .vertical_addressable = 1050,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 39,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 5995,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x3B) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x3B);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 18700,
      .horizontal_addressable = 1680,
      .horizontal_front_porch = 120,
      .horizontal_sync_pulse = 176,
      .horizontal_blanking = 592,
      .vertical_addressable = 1050,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 49,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 7489,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x3C) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x3C);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 21475,
      .horizontal_addressable = 1680,
      .horizontal_front_porch = 128,
      .horizontal_sync_pulse = 176,
      .horizontal_blanking = 608,
      .vertical_addressable = 1050,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 55,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 8494,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x3D) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x3D);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 24550,
      .horizontal_addressable = 1680,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 1050,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 62,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 11999,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x3E) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x3E);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 20475,
      .horizontal_addressable = 1792,
      .horizontal_front_porch = 128,
      .horizontal_sync_pulse = 200,
      .horizontal_blanking = 656,
      .vertical_addressable = 1344,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 50,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 6000,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x3F) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x3F);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 26100,
      .horizontal_addressable = 1792,
      .horizontal_front_porch = 96,
      .horizontal_sync_pulse = 216,
      .horizontal_blanking = 664,
      .vertical_addressable = 1344,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 73,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 7500,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x40) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x40);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 33325,
      .horizontal_addressable = 1792,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 1344,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 4,
      .vertical_blanking = 79,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 11997,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x41) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x41);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 21825,
      .horizontal_addressable = 1856,
      .horizontal_front_porch = 96,
      .horizontal_sync_pulse = 224,
      .horizontal_blanking = 672,
      .vertical_addressable = 1392,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 47,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 6000,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x42) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x42);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 28800,
      .horizontal_addressable = 1856,
      .horizontal_front_porch = 128,
      .horizontal_sync_pulse = 224,
      .horizontal_blanking = 704,
      .vertical_addressable = 1392,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 108,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 7500,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x43) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x43);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 35650,
      .horizontal_addressable = 1856,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 1392,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 4,
      .vertical_blanking = 82,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 11997,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x44) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x44);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 15400,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 1200,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 35,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 5995,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x45) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x45);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 19325,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 136,
      .horizontal_sync_pulse = 200,
      .horizontal_blanking = 672,
      .vertical_addressable = 1200,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 5989,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x46) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x46);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 24525,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 136,
      .horizontal_sync_pulse = 208,
      .horizontal_blanking = 688,
      .vertical_addressable = 1200,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 55,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 7493,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x47) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x47);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 28125,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 144,
      .horizontal_sync_pulse = 208,
      .horizontal_blanking = 704,
      .vertical_addressable = 1200,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 62,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 8493,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x48) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x48);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 31700,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 1200,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 71,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 11991,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x49) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x49);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 23400,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 128,
      .horizontal_sync_pulse = 208,
      .horizontal_blanking = 680,
      .vertical_addressable = 1440,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 60,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 6000,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x4A) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x4A);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 29700,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 144,
      .horizontal_sync_pulse = 224,
      .horizontal_blanking = 720,
      .vertical_addressable = 1440,
      .vertical_front_porch = 1,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 60,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 7500,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x4B) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x4B);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 38050,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 1440,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 4,
      .vertical_blanking = 85,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 11996,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x4C) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x4C);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 26850,
      .horizontal_addressable = 2560,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 1600,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 46,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 5997,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x4D) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x4D);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 34850,
      .horizontal_addressable = 2560,
      .horizontal_front_porch = 192,
      .horizontal_sync_pulse = 280,
      .horizontal_blanking = 944,
      .vertical_addressable = 1600,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 58,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 5999,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x4E) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x4E);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 44325,
      .horizontal_addressable = 2560,
      .horizontal_front_porch = 208,
      .horizontal_sync_pulse = 280,
      .horizontal_blanking = 976,
      .vertical_addressable = 1600,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 72,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 7497,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x4F) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x4F);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 50525,
      .horizontal_addressable = 2560,
      .horizontal_front_porch = 208,
      .horizontal_sync_pulse = 280,
      .horizontal_blanking = 976,
      .vertical_addressable = 1600,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 82,
      .flags = timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 8495,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

TEST(ConvertedDmtTimingMatchesTimingParams, DmtId0x50) {
  const DmtTiming& kDmtTiming = FindDmtTimingById(0x50);
  const timing_params_t kConverted = ToTimingParams(kDmtTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 55275,
      .horizontal_addressable = 2560,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 32,
      .horizontal_blanking = 160,
      .vertical_addressable = 1600,
      .vertical_front_porch = 3,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 94,
      .flags = timing_params_t::kPositiveHsync,
      .vertical_refresh_e2 = 11996,
  };

  EXPECT_EQ(kExpected.pixel_freq_10khz, kConverted.pixel_freq_10khz);
  EXPECT_EQ(kExpected.horizontal_addressable, kConverted.horizontal_addressable);
  EXPECT_EQ(kExpected.horizontal_front_porch, kConverted.horizontal_front_porch);
  EXPECT_EQ(kExpected.horizontal_sync_pulse, kConverted.horizontal_sync_pulse);
  EXPECT_EQ(kExpected.horizontal_blanking, kConverted.horizontal_blanking);
  EXPECT_EQ(kExpected.vertical_addressable, kConverted.vertical_addressable);
  EXPECT_EQ(kExpected.vertical_front_porch, kConverted.vertical_front_porch);
  EXPECT_EQ(kExpected.vertical_sync_pulse, kConverted.vertical_sync_pulse);
  EXPECT_EQ(kExpected.vertical_blanking, kConverted.vertical_blanking);
  EXPECT_EQ(kExpected.flags, kConverted.flags);
  EXPECT_EQ(kExpected.vertical_refresh_e2, kConverted.vertical_refresh_e2);
}

}  // namespace

}  // namespace edid::internal
