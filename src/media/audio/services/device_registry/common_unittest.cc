// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/common_unittest.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>

#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/validate.h"

namespace media_audio {

////////////////////////////////////////////////////////////////////////////////////////////////
// Helper functions that are useful for both low- (Device) and high-level (AdrServer) unittests.
//

////////////////////////
// Codec-related methods
fuchsia_hardware_audio::DaiFormat SafeDaiFormatFromDaiSupportedFormats(
    const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets) {
  FX_CHECK(!dai_format_sets.empty()) << "empty DaiSupportedFormats";

  FX_CHECK(
      !dai_format_sets[0].number_of_channels().empty() &&
      !dai_format_sets[0].sample_formats().empty() && !dai_format_sets[0].frame_formats().empty() &&
      !dai_format_sets[0].frame_rates().empty() && !dai_format_sets[0].bits_per_slot().empty() &&
      !dai_format_sets[0].bits_per_sample().empty())
      << "empty sub-vector in DaiSupportedFormats";

  fuchsia_hardware_audio::DaiFormat dai_format{{
      .number_of_channels = dai_format_sets[0].number_of_channels()[0],
      .channels_to_use_bitmask = (dai_format_sets[0].number_of_channels()[0] < 64
                                      ? (1ull << dai_format_sets[0].number_of_channels()[0]) - 1ull
                                      : 0xFFFFFFFFFFFFFFFFull),
      .sample_format = dai_format_sets[0].sample_formats()[0],
      .frame_format = dai_format_sets[0].frame_formats()[0],
      .frame_rate = dai_format_sets[0].frame_rates()[0],
      .bits_per_slot = dai_format_sets[0].bits_per_slot()[0],
      .bits_per_sample = dai_format_sets[0].bits_per_sample()[0],
  }};
  FX_CHECK(ValidateDaiFormat(dai_format) == ZX_OK)
      << "first entries did not create a valid DaiFormat";

  return dai_format;
}

fuchsia_hardware_audio::DaiFormat SecondDaiFormatFromDaiSupportedFormats(
    const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets) {
  auto safe_format_2 = SafeDaiFormatFromDaiSupportedFormats(dai_format_sets);

  if (safe_format_2.channels_to_use_bitmask() > 1) {
    safe_format_2.channels_to_use_bitmask() -= 1;
  } else if (dai_format_sets[0].number_of_channels().size() > 1) {
    safe_format_2.number_of_channels() = dai_format_sets[0].number_of_channels()[1];
  } else if (dai_format_sets[0].sample_formats().size() > 1) {
    safe_format_2.sample_format() = dai_format_sets[0].sample_formats()[1];
  } else if (dai_format_sets[0].frame_formats().size() > 1) {
    safe_format_2.frame_format() = dai_format_sets[0].frame_formats()[1];
  } else if (dai_format_sets[0].frame_rates().size() > 1) {
    safe_format_2.frame_rate() = dai_format_sets[0].frame_rates()[1];
  } else if (dai_format_sets[0].bits_per_slot().size() > 1) {
    safe_format_2.bits_per_slot() = dai_format_sets[0].bits_per_slot()[1];
  } else if (dai_format_sets[0].bits_per_sample().size() > 1) {
    safe_format_2.bits_per_sample() = dai_format_sets[0].bits_per_sample()[1];
  } else if (dai_format_sets.size() > 1) {
    return fuchsia_hardware_audio::DaiFormat{{
        .number_of_channels = dai_format_sets[1].number_of_channels()[0],
        .channels_to_use_bitmask =
            (dai_format_sets[1].number_of_channels()[0] < 64
                 ? (1ull << dai_format_sets[1].number_of_channels()[0]) - 1ull
                 : 0xFFFFFFFFFFFFFFFFull),
        .sample_format = dai_format_sets[1].sample_formats()[0],
        .frame_format = dai_format_sets[1].frame_formats()[0],
        .frame_rate = dai_format_sets[1].frame_rates()[0],
        .bits_per_slot = dai_format_sets[1].bits_per_slot()[0],
        .bits_per_sample = dai_format_sets[1].bits_per_sample()[0],
    }};

  } else {
    FX_CHECK(false) << "Dai format set has only one possible valid format";
  }
  return safe_format_2;
}

fuchsia_hardware_audio::DaiFormat UnsupportedDaiFormatFromDaiFormatSets(
    const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets) {
  FX_CHECK(!dai_format_sets.empty()) << "empty DaiSupportedFormats";

  std::optional<fuchsia_hardware_audio::DaiFormat> dai_format;
  for (const auto& format_set : dai_format_sets) {
    FX_CHECK(!format_set.number_of_channels().empty() && !format_set.sample_formats().empty() &&
             !format_set.frame_formats().empty() && !format_set.frame_rates().empty() &&
             !format_set.bits_per_slot().empty() && !format_set.bits_per_sample().empty())
        << "empty sub-vector in DaiSupportedFormats";
    dai_format = SafeDaiFormatFromDaiSupportedFormats({{format_set}});
    if (dai_format->number_of_channels() > 1) {
      dai_format->number_of_channels() -= 1;
      dai_format->channels_to_use_bitmask() = (1ull << dai_format->number_of_channels()) - 1ull;
      FX_LOGS(INFO) << "Returning this invalid format: ";
      LogDaiFormat(dai_format);
      return *dai_format;
    }
    if (dai_format->frame_rate() > kMinSupportedDaiFrameRate) {
      dai_format->frame_rate() -= 1;
      FX_LOGS(INFO) << "Returning this invalid format: ";
      LogDaiFormat(dai_format);
      return *dai_format;
    }
    if (dai_format->bits_per_slot() > 1) {
      dai_format->bits_per_slot() -= 1;
      FX_LOGS(INFO) << "Returning this invalid format: ";
      LogDaiFormat(dai_format);
      return *dai_format;
    }
    if (dai_format->bits_per_sample() > 1) {
      dai_format->bits_per_sample() -= 1;
      FX_LOGS(INFO) << "Returning this invalid format: ";
      LogDaiFormat(dai_format);
      return *dai_format;
    }
  }

  FX_CHECK(false) << "No invalid DaiFormat found for these format_sets";
  __UNREACHABLE;
}

}  // namespace media_audio
