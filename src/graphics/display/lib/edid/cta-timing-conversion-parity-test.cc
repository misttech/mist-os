// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/graphics/display/lib/edid/cta-timing.h"
#include "src/graphics/display/lib/edid/timings.h"

namespace edid::internal {

namespace {

const CtaTiming& FindCtaTimingById(int32_t id) {
  const auto cta_timing_it =
      std::find_if(kCtaTimings.begin(), kCtaTimings.end(),
                   [id](const CtaTiming& cta) { return cta.video_identification_code == id; });
  ZX_ASSERT(cta_timing_it != kCtaTimings.end());
  return *cta_timing_it;
}

// TODO(liyl): The following `ConvertedCtaTimingMatchesTimingParams` tests are
// only used as one-offs in order to make sure that there's no binary change to
// the `timing_params_t` array for all CTA timings. We can delete them once the
// code is merged.

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic1) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(1);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic2) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(2);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 2700,
      .horizontal_addressable = 720,
      .horizontal_front_porch = 16,
      .horizontal_sync_pulse = 62,
      .horizontal_blanking = 138,
      .vertical_addressable = 480,
      .vertical_front_porch = 9,
      .vertical_sync_pulse = 6,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic3) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(3);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 2700,
      .horizontal_addressable = 720,
      .horizontal_front_porch = 16,
      .horizontal_sync_pulse = 62,
      .horizontal_blanking = 138,
      .vertical_addressable = 480,
      .vertical_front_porch = 9,
      .vertical_sync_pulse = 6,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic4) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(4);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7425,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 110,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 370,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic5) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(5);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7425,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 88,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 280,
      .vertical_addressable = 1080,
      .vertical_front_porch = 2,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 22,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync |
               timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic6) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(6);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 2700,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 38,
      .horizontal_sync_pulse = 124,
      .horizontal_blanking = 276,
      .vertical_addressable = 480,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 22,
      .flags = timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank |
               timing_params_t::kDoubleClocked,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic7) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(7);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 2700,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 38,
      .horizontal_sync_pulse = 124,
      .horizontal_blanking = 276,
      .vertical_addressable = 480,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 22,
      .flags = timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank |
               timing_params_t::kDoubleClocked,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic8) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(8);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 2700,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 38,
      .horizontal_sync_pulse = 124,
      .horizontal_blanking = 276,
      .vertical_addressable = 240,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 22,
      .flags = timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 6005,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic9) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(9);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 2700,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 38,
      .horizontal_sync_pulse = 124,
      .horizontal_blanking = 276,
      .vertical_addressable = 240,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 22,
      .flags = timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 6005,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic10) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(10);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 2880,
      .horizontal_front_porch = 76,
      .horizontal_sync_pulse = 248,
      .horizontal_blanking = 552,
      .vertical_addressable = 480,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 22,
      .flags = timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank |
               timing_params_t::kDoubleClocked,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic11) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(11);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 2880,
      .horizontal_front_porch = 76,
      .horizontal_sync_pulse = 248,
      .horizontal_blanking = 552,
      .vertical_addressable = 480,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 22,
      .flags = timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank |
               timing_params_t::kDoubleClocked,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic12) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(12);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 2880,
      .horizontal_front_porch = 76,
      .horizontal_sync_pulse = 248,
      .horizontal_blanking = 552,
      .vertical_addressable = 240,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 22,
      .flags = timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 6005,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic13) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(13);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 2880,
      .horizontal_front_porch = 76,
      .horizontal_sync_pulse = 248,
      .horizontal_blanking = 552,
      .vertical_addressable = 240,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 22,
      .flags = timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 6005,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic14) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(14);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 32,
      .horizontal_sync_pulse = 124,
      .horizontal_blanking = 276,
      .vertical_addressable = 480,
      .vertical_front_porch = 9,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 45,
      .flags = timing_params_t::kDoubleClocked,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic15) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(15);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 32,
      .horizontal_sync_pulse = 124,
      .horizontal_blanking = 276,
      .vertical_addressable = 480,
      .vertical_front_porch = 9,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 45,
      .flags = timing_params_t::kDoubleClocked,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic16) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(16);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 14850,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 88,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 280,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic17) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(17);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 2700,
      .horizontal_addressable = 720,
      .horizontal_front_porch = 12,
      .horizontal_sync_pulse = 64,
      .horizontal_blanking = 144,
      .vertical_addressable = 576,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 49,
      .flags = 0,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic18) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(18);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 2700,
      .horizontal_addressable = 720,
      .horizontal_front_porch = 12,
      .horizontal_sync_pulse = 64,
      .horizontal_blanking = 144,
      .vertical_addressable = 576,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 49,
      .flags = 0,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic19) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(19);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7425,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 440,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 700,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic20) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(20);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7425,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 528,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 720,
      .vertical_addressable = 1080,
      .vertical_front_porch = 2,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 22,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync |
               timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic21) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(21);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 2700,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 24,
      .horizontal_sync_pulse = 126,
      .horizontal_blanking = 288,
      .vertical_addressable = 576,
      .vertical_front_porch = 2,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 24,
      .flags = timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank |
               timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic22) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(22);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 2700,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 24,
      .horizontal_sync_pulse = 126,
      .horizontal_blanking = 288,
      .vertical_addressable = 576,
      .vertical_front_porch = 2,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 24,
      .flags = timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank |
               timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic23) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(23);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 2700,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 24,
      .horizontal_sync_pulse = 126,
      .horizontal_blanking = 288,
      .vertical_addressable = 288,
      .vertical_front_porch = 2,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 24,
      .flags = timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 5008,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic24) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(24);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 2700,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 24,
      .horizontal_sync_pulse = 126,
      .horizontal_blanking = 288,
      .vertical_addressable = 288,
      .vertical_front_porch = 2,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 24,
      .flags = timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 5008,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic25) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(25);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 2880,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 252,
      .horizontal_blanking = 576,
      .vertical_addressable = 576,
      .vertical_front_porch = 2,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 24,
      .flags = timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank |
               timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic26) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(26);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 2880,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 252,
      .horizontal_blanking = 576,
      .vertical_addressable = 576,
      .vertical_front_porch = 2,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 24,
      .flags = timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank |
               timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic27) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(27);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 2880,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 252,
      .horizontal_blanking = 576,
      .vertical_addressable = 288,
      .vertical_front_porch = 2,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 24,
      .flags = timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 5008,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic28) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(28);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 2880,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 252,
      .horizontal_blanking = 576,
      .vertical_addressable = 288,
      .vertical_front_porch = 2,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 24,
      .flags = timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 5008,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic29) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(29);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 24,
      .horizontal_sync_pulse = 128,
      .horizontal_blanking = 288,
      .vertical_addressable = 576,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 49,
      .flags = timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic30) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(30);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 24,
      .horizontal_sync_pulse = 128,
      .horizontal_blanking = 288,
      .vertical_addressable = 576,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 49,
      .flags = timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic31) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(31);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 14850,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 528,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 720,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic32) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(32);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7425,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 638,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 830,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2400,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic33) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(33);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7425,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 528,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 720,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2500,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic34) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(34);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7425,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 88,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 280,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 3000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic35) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(35);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10800,
      .horizontal_addressable = 2880,
      .horizontal_front_porch = 64,
      .horizontal_sync_pulse = 248,
      .horizontal_blanking = 552,
      .vertical_addressable = 480,
      .vertical_front_porch = 9,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 45,
      .flags = timing_params_t::kDoubleClocked,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic36) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(36);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10800,
      .horizontal_addressable = 2880,
      .horizontal_front_porch = 64,
      .horizontal_sync_pulse = 248,
      .horizontal_blanking = 552,
      .vertical_addressable = 480,
      .vertical_front_porch = 9,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 45,
      .flags = timing_params_t::kDoubleClocked,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic37) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(37);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10800,
      .horizontal_addressable = 2880,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 256,
      .horizontal_blanking = 576,
      .vertical_addressable = 576,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 49,
      .flags = timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic38) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(38);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10800,
      .horizontal_addressable = 2880,
      .horizontal_front_porch = 48,
      .horizontal_sync_pulse = 256,
      .horizontal_blanking = 576,
      .vertical_addressable = 576,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 49,
      .flags = timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic39) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(39);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7200,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 32,
      .horizontal_sync_pulse = 168,
      .horizontal_blanking = 384,
      .vertical_addressable = 1080,
      .vertical_front_porch = 23,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 85,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kInterlaced,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic40) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(40);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 14850,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 528,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 720,
      .vertical_addressable = 1080,
      .vertical_front_porch = 2,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 22,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync |
               timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank,
      .vertical_refresh_e2 = 10000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic41) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(41);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 14850,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 440,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 700,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 10000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic42) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(42);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 720,
      .horizontal_front_porch = 12,
      .horizontal_sync_pulse = 64,
      .horizontal_blanking = 144,
      .vertical_addressable = 576,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 49,
      .flags = 0,
      .vertical_refresh_e2 = 10000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic43) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(43);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 720,
      .horizontal_front_porch = 12,
      .horizontal_sync_pulse = 64,
      .horizontal_blanking = 144,
      .vertical_addressable = 576,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 49,
      .flags = 0,
      .vertical_refresh_e2 = 10000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic44) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(44);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 24,
      .horizontal_sync_pulse = 126,
      .horizontal_blanking = 288,
      .vertical_addressable = 576,
      .vertical_front_porch = 2,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 24,
      .flags = timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank |
               timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 10000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic45) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(45);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 24,
      .horizontal_sync_pulse = 126,
      .horizontal_blanking = 288,
      .vertical_addressable = 576,
      .vertical_front_porch = 2,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 24,
      .flags = timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank |
               timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 10000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic46) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(46);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 14850,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 88,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 280,
      .vertical_addressable = 1080,
      .vertical_front_porch = 2,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 22,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync |
               timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank,
      .vertical_refresh_e2 = 12000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic47) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(47);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 14850,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 110,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 370,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 12000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic48) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(48);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 720,
      .horizontal_front_porch = 16,
      .horizontal_sync_pulse = 62,
      .horizontal_blanking = 138,
      .vertical_addressable = 480,
      .vertical_front_porch = 9,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 45,
      .flags = 0,
      .vertical_refresh_e2 = 11988,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic49) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(49);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 720,
      .horizontal_front_porch = 16,
      .horizontal_sync_pulse = 62,
      .horizontal_blanking = 138,
      .vertical_addressable = 480,
      .vertical_front_porch = 9,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 45,
      .flags = 0,
      .vertical_refresh_e2 = 11988,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic50) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(50);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 38,
      .horizontal_sync_pulse = 124,
      .horizontal_blanking = 276,
      .vertical_addressable = 480,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 22,
      .flags = timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank |
               timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 11988,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic51) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(51);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5400,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 38,
      .horizontal_sync_pulse = 124,
      .horizontal_blanking = 276,
      .vertical_addressable = 480,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 22,
      .flags = timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank |
               timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 11988,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic52) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(52);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10800,
      .horizontal_addressable = 720,
      .horizontal_front_porch = 12,
      .horizontal_sync_pulse = 64,
      .horizontal_blanking = 144,
      .vertical_addressable = 576,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 49,
      .flags = 0,
      .vertical_refresh_e2 = 20000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic53) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(53);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10800,
      .horizontal_addressable = 720,
      .horizontal_front_porch = 12,
      .horizontal_sync_pulse = 64,
      .horizontal_blanking = 144,
      .vertical_addressable = 576,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 49,
      .flags = 0,
      .vertical_refresh_e2 = 20000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic54) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(54);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10800,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 24,
      .horizontal_sync_pulse = 126,
      .horizontal_blanking = 288,
      .vertical_addressable = 576,
      .vertical_front_porch = 2,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 24,
      .flags = timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank |
               timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 20000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic55) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(55);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10800,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 24,
      .horizontal_sync_pulse = 126,
      .horizontal_blanking = 288,
      .vertical_addressable = 576,
      .vertical_front_porch = 2,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 24,
      .flags = timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank |
               timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 20000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic56) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(56);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10800,
      .horizontal_addressable = 720,
      .horizontal_front_porch = 16,
      .horizontal_sync_pulse = 62,
      .horizontal_blanking = 138,
      .vertical_addressable = 480,
      .vertical_front_porch = 9,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 45,
      .flags = 0,
      .vertical_refresh_e2 = 23976,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic57) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(57);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10800,
      .horizontal_addressable = 720,
      .horizontal_front_porch = 16,
      .horizontal_sync_pulse = 62,
      .horizontal_blanking = 138,
      .vertical_addressable = 480,
      .vertical_front_porch = 9,
      .vertical_sync_pulse = 6,
      .vertical_blanking = 45,
      .flags = 0,
      .vertical_refresh_e2 = 23976,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic58) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(58);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10800,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 38,
      .horizontal_sync_pulse = 124,
      .horizontal_blanking = 276,
      .vertical_addressable = 480,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 22,
      .flags = timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank |
               timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 23976,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic59) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(59);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 10800,
      .horizontal_addressable = 1440,
      .horizontal_front_porch = 38,
      .horizontal_sync_pulse = 124,
      .horizontal_blanking = 276,
      .vertical_addressable = 480,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 3,
      .vertical_blanking = 22,
      .flags = timing_params_t::kInterlaced | timing_params_t::kAlternatingVblank |
               timing_params_t::kDoubleClocked,
      .vertical_refresh_e2 = 23976,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic60) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(60);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5940,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 1760,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 2020,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2400,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic61) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(61);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7425,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 2420,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 2680,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2500,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic62) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(62);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7425,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 1760,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 2020,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 3000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic63) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(63);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 29700,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 88,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 280,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 12000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic64) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(64);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 29700,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 528,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 720,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 10000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic65) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(65);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5940,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 1760,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 2020,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2400,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic66) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(66);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7425,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 2420,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 2680,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2500,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic67) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(67);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7425,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 1760,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 2020,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 3000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic68) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(68);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7425,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 440,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 700,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic69) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(69);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7425,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 110,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 370,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic70) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(70);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 14850,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 440,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 700,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 10000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic71) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(71);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 14850,
      .horizontal_addressable = 1280,
      .horizontal_front_porch = 110,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 370,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 12000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic72) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(72);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7425,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 638,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 830,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2400,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic73) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(73);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7425,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 528,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 720,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2500,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic74) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(74);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 7425,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 88,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 280,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 3000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic75) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(75);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 14850,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 528,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 720,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic76) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(76);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 14850,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 88,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 280,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic77) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(77);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 29700,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 528,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 720,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 10000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic78) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(78);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 29700,
      .horizontal_addressable = 1920,
      .horizontal_front_porch = 88,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 280,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 12000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic79) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(79);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5940,
      .horizontal_addressable = 1680,
      .horizontal_front_porch = 1360,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 1620,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2400,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic80) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(80);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5940,
      .horizontal_addressable = 1680,
      .horizontal_front_porch = 1228,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 1488,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2500,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic81) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(81);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 5940,
      .horizontal_addressable = 1680,
      .horizontal_front_porch = 700,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 960,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 3000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic82) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(82);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 8250,
      .horizontal_addressable = 1680,
      .horizontal_front_porch = 260,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 520,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic83) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(83);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 9900,
      .horizontal_addressable = 1680,
      .horizontal_front_porch = 260,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 520,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 30,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic84) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(84);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 16500,
      .horizontal_addressable = 1680,
      .horizontal_front_porch = 60,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 320,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 105,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 10000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic85) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(85);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 19800,
      .horizontal_addressable = 1680,
      .horizontal_front_porch = 60,
      .horizontal_sync_pulse = 40,
      .horizontal_blanking = 320,
      .vertical_addressable = 720,
      .vertical_front_porch = 5,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 105,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 12000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic86) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(86);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 9900,
      .horizontal_addressable = 2560,
      .horizontal_front_porch = 998,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 1190,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 20,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2400,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic87) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(87);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 9000,
      .horizontal_addressable = 2560,
      .horizontal_front_porch = 448,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 640,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2500,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic88) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(88);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 11880,
      .horizontal_addressable = 2560,
      .horizontal_front_porch = 768,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 960,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 3000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic89) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(89);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 18563,
      .horizontal_addressable = 2560,
      .horizontal_front_porch = 548,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 740,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 45,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic90) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(90);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 19800,
      .horizontal_addressable = 2560,
      .horizontal_front_porch = 248,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 440,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 20,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic91) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(91);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 37125,
      .horizontal_addressable = 2560,
      .horizontal_front_porch = 218,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 410,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 170,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 10000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic92) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(92);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 49500,
      .horizontal_addressable = 2560,
      .horizontal_front_porch = 548,
      .horizontal_sync_pulse = 44,
      .horizontal_blanking = 740,
      .vertical_addressable = 1080,
      .vertical_front_porch = 4,
      .vertical_sync_pulse = 5,
      .vertical_blanking = 170,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 12000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic93) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(93);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 29700,
      .horizontal_addressable = 3840,
      .horizontal_front_porch = 1276,
      .horizontal_sync_pulse = 88,
      .horizontal_blanking = 1660,
      .vertical_addressable = 2160,
      .vertical_front_porch = 8,
      .vertical_sync_pulse = 10,
      .vertical_blanking = 90,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2400,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic94) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(94);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 29700,
      .horizontal_addressable = 3840,
      .horizontal_front_porch = 1056,
      .horizontal_sync_pulse = 88,
      .horizontal_blanking = 1440,
      .vertical_addressable = 2160,
      .vertical_front_porch = 8,
      .vertical_sync_pulse = 10,
      .vertical_blanking = 90,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2500,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic95) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(95);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 29700,
      .horizontal_addressable = 3840,
      .horizontal_front_porch = 176,
      .horizontal_sync_pulse = 88,
      .horizontal_blanking = 560,
      .vertical_addressable = 2160,
      .vertical_front_porch = 8,
      .vertical_sync_pulse = 10,
      .vertical_blanking = 90,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 3000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic96) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(96);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 59400,
      .horizontal_addressable = 3840,
      .horizontal_front_porch = 1056,
      .horizontal_sync_pulse = 88,
      .horizontal_blanking = 1440,
      .vertical_addressable = 2160,
      .vertical_front_porch = 8,
      .vertical_sync_pulse = 10,
      .vertical_blanking = 90,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic97) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(97);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 59400,
      .horizontal_addressable = 3840,
      .horizontal_front_porch = 176,
      .horizontal_sync_pulse = 88,
      .horizontal_blanking = 560,
      .vertical_addressable = 2160,
      .vertical_front_porch = 8,
      .vertical_sync_pulse = 10,
      .vertical_blanking = 90,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic98) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(98);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 29700,
      .horizontal_addressable = 4096,
      .horizontal_front_porch = 1020,
      .horizontal_sync_pulse = 88,
      .horizontal_blanking = 1404,
      .vertical_addressable = 2160,
      .vertical_front_porch = 8,
      .vertical_sync_pulse = 10,
      .vertical_blanking = 90,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2400,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic99) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(99);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 29700,
      .horizontal_addressable = 4096,
      .horizontal_front_porch = 968,
      .horizontal_sync_pulse = 88,
      .horizontal_blanking = 1184,
      .vertical_addressable = 2160,
      .vertical_front_porch = 8,
      .vertical_sync_pulse = 10,
      .vertical_blanking = 90,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2500,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic100) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(100);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 29700,
      .horizontal_addressable = 4096,
      .horizontal_front_porch = 88,
      .horizontal_sync_pulse = 88,
      .horizontal_blanking = 304,
      .vertical_addressable = 2160,
      .vertical_front_porch = 8,
      .vertical_sync_pulse = 10,
      .vertical_blanking = 90,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 3000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic101) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(101);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 59400,
      .horizontal_addressable = 4096,
      .horizontal_front_porch = 968,
      .horizontal_sync_pulse = 88,
      .horizontal_blanking = 1184,
      .vertical_addressable = 2160,
      .vertical_front_porch = 8,
      .vertical_sync_pulse = 10,
      .vertical_blanking = 90,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic102) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(102);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 59400,
      .horizontal_addressable = 4096,
      .horizontal_front_porch = 88,
      .horizontal_sync_pulse = 88,
      .horizontal_blanking = 304,
      .vertical_addressable = 2160,
      .vertical_front_porch = 8,
      .vertical_sync_pulse = 10,
      .vertical_blanking = 90,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic103) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(103);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 29700,
      .horizontal_addressable = 3840,
      .horizontal_front_porch = 1276,
      .horizontal_sync_pulse = 88,
      .horizontal_blanking = 1660,
      .vertical_addressable = 2160,
      .vertical_front_porch = 8,
      .vertical_sync_pulse = 10,
      .vertical_blanking = 90,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2400,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic104) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(104);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 29700,
      .horizontal_addressable = 3840,
      .horizontal_front_porch = 1056,
      .horizontal_sync_pulse = 88,
      .horizontal_blanking = 1440,
      .vertical_addressable = 2160,
      .vertical_front_porch = 8,
      .vertical_sync_pulse = 10,
      .vertical_blanking = 90,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 2500,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic105) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(105);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 29700,
      .horizontal_addressable = 3840,
      .horizontal_front_porch = 176,
      .horizontal_sync_pulse = 88,
      .horizontal_blanking = 560,
      .vertical_addressable = 2160,
      .vertical_front_porch = 8,
      .vertical_sync_pulse = 10,
      .vertical_blanking = 90,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 3000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic106) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(106);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 59400,
      .horizontal_addressable = 3840,
      .horizontal_front_porch = 1056,
      .horizontal_sync_pulse = 88,
      .horizontal_blanking = 1440,
      .vertical_addressable = 2160,
      .vertical_front_porch = 8,
      .vertical_sync_pulse = 10,
      .vertical_blanking = 90,
      .flags = timing_params_t::kPositiveHsync | timing_params_t::kPositiveVsync,
      .vertical_refresh_e2 = 5000,
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

TEST(ConvertedCtaTimingMatchesTimingParams, CtaVic107) {
  const CtaTiming& kCtaTiming = FindCtaTimingById(107);
  const timing_params_t kConverted = ToTimingParams(kCtaTiming);
  static constexpr timing_params_t kExpected = {
      .pixel_freq_10khz = 59400,
      .horizontal_addressable = 3840,
      .horizontal_front_porch = 176,
      .horizontal_sync_pulse = 88,
      .horizontal_blanking = 560,
      .vertical_addressable = 2160,
      .vertical_front_porch = 8,
      .vertical_sync_pulse = 10,
      .vertical_blanking = 90,
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

}  // namespace

}  // namespace edid::internal
