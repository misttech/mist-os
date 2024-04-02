// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/common_unittest.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/validate.h"

namespace media_audio {

////////////////////////////////////////////////////////////////////////////////////////////////
// Helper functions that are useful for both low- (Device) and high-level (AdrServer) unittests.
//

////////////////////////
// Codec-related methods
fuchsia_hardware_audio::DaiFormat SafeDaiFormatFromElementDaiFormatSets(
    const std::vector<fuchsia_audio_device::ElementDaiFormatSet>& element_dai_format_sets,
    ElementId element_id) {
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_format_sets;
  for (const auto& element_entry : element_dai_format_sets) {
    if (element_entry.element_id() && *element_entry.element_id() == element_id) {
      return SafeDaiFormatFromDaiFormatSets(*element_entry.format_sets());
    }
  }
  ADD_FAILURE()
      << "SafeDaiFormatFromDaiFormatSets: No element_dai_format_sets entry found with specified element_id "
      << element_id;
  return {{}};
}

fuchsia_hardware_audio::DaiFormat SecondDaiFormatFromElementDaiFormatSets(
    const std::vector<fuchsia_audio_device::ElementDaiFormatSet>& element_dai_format_sets,
    ElementId element_id) {
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_format_sets;
  for (const auto& element_entry : element_dai_format_sets) {
    if (element_entry.element_id() && *element_entry.element_id() == element_id) {
      return SecondDaiFormatFromDaiFormatSets(*element_entry.format_sets());
    }
  }
  ADD_FAILURE() << "Could not create a second valid DaiFormat for specified element_id "
                << element_id;
  return {{}};
}

fuchsia_hardware_audio::DaiFormat UnsupportedDaiFormatFromElementDaiFormatSets(
    const std::vector<fuchsia_audio_device::ElementDaiFormatSet>& element_dai_format_sets,
    ElementId element_id) {
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_format_sets;
  for (const auto& element_entry : element_dai_format_sets) {
    if (element_entry.element_id() && *element_entry.element_id() == element_id) {
      return UnsupportedDaiFormatFromDaiFormatSets(*element_entry.format_sets());
    }
  }
  ADD_FAILURE()
      << "UnsupportedDaiFormatFromDaiFormatSets could not find an invalid dai_format for element_id "
      << element_id;
  return {{}};
}

fuchsia_hardware_audio::DaiFormat SafeDaiFormatFromDaiFormatSets(
    const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets) {
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
  if (ValidateDaiFormat(dai_format) != ZX_OK) {
    ADD_FAILURE() << "first entries did not create a valid DaiFormat";
  }

  return dai_format;
}

fuchsia_hardware_audio::DaiFormat SecondDaiFormatFromDaiFormatSets(
    const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets) {
  auto safe_format_2 = SafeDaiFormatFromDaiFormatSets(dai_format_sets);

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
    ADD_FAILURE() << "Dai format set has only one possible valid format";
    return {{}};
  }
  if (ValidateDaiFormat(safe_format_2) != ZX_OK) {
    ADD_FAILURE() << "Could not create a second valid DaiFormat";
  }
  return safe_format_2;
}

fuchsia_hardware_audio::DaiFormat UnsupportedDaiFormatFromDaiFormatSets(
    const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets) {
  auto dai_format = SafeDaiFormatFromDaiFormatSets(dai_format_sets);
  if (dai_format.number_of_channels() > 1) {
    dai_format.number_of_channels() -= 1;
    dai_format.channels_to_use_bitmask() = (1ull << dai_format.number_of_channels()) - 1ull;
    FX_LOGS(INFO) << "Returning this invalid format: ";
    LogDaiFormat(dai_format);
    return dai_format;
  }
  if (dai_format.frame_rate() > kMinSupportedDaiFrameRate) {
    dai_format.frame_rate() -= 1;
    FX_LOGS(INFO) << "Returning this invalid format: ";
    LogDaiFormat(dai_format);
    return dai_format;
  }
  if (dai_format.bits_per_slot() > 1) {
    dai_format.bits_per_slot() -= 1;
    FX_LOGS(INFO) << "Returning this invalid format: ";
    LogDaiFormat(dai_format);
    return dai_format;
  }
  if (dai_format.bits_per_sample() > 1) {
    dai_format.bits_per_sample() -= 1;
    FX_LOGS(INFO) << "Returning this invalid format: ";
    LogDaiFormat(dai_format);
    return dai_format;
  }

  ADD_FAILURE() << "No invalid DaiFormat found for these format_sets";
  return {{}};
}

}  // namespace media_audio
