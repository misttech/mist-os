// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/common_unittest.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/validate.h"

namespace media_audio {

namespace fad = fuchsia_audio_device;
namespace fha = fuchsia_hardware_audio;

////////////////////////////////////////////////////////////////////////////////////////////////
// Helper functions that are useful for both low- (Device) and high-level (AdrServer) unittests.
//

///////////////////////////////
// Codec-related functions
//
// From a multi-element collection, each with many DaiSupportedFormats, get a DaiFormat.
fha::DaiFormat SafeDaiFormatFromElementDaiFormatSets(
    ElementId element_id, const std::vector<fad::ElementDaiFormatSet>& element_dai_format_sets) {
  std::vector<fha::DaiSupportedFormats> dai_format_sets;
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

// From a multi-element collection, each with many DaiSupportedFormats, get a DIFFERENT DaiFormat.
fha::DaiFormat SecondDaiFormatFromElementDaiFormatSets(
    ElementId element_id, const std::vector<fad::ElementDaiFormatSet>& element_dai_format_sets) {
  std::vector<fha::DaiSupportedFormats> dai_format_sets;
  for (const auto& element_entry : element_dai_format_sets) {
    if (element_entry.element_id() && *element_entry.element_id() == element_id) {
      return SecondDaiFormatFromDaiFormatSets(*element_entry.format_sets());
    }
  }
  ADD_FAILURE() << "Could not create a second valid DaiFormat for specified element_id "
                << element_id;
  return {{}};
}

// From a multi-element collection, each with many DaiSupportedFormats,
// get a DaiFormat that is UNSUPPORTED (but still a valid format).
fha::DaiFormat UnsupportedDaiFormatFromElementDaiFormatSets(
    ElementId element_id, const std::vector<fad::ElementDaiFormatSet>& element_dai_format_sets) {
  std::vector<fha::DaiSupportedFormats> dai_format_sets;
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

// From many DaiSupportedFormats, get a DaiFormat.
fha::DaiFormat SafeDaiFormatFromDaiFormatSets(
    const std::vector<fha::DaiSupportedFormats>& dai_format_sets) {
  fha::DaiFormat dai_format{{
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
  if (!ValidateDaiFormat(dai_format)) {
    ADD_FAILURE() << "first entries did not create a valid DaiFormat";
  }

  return dai_format;
}

// From many DaiSupportedFormats, get a DIFFERENT DaiFormat.
fha::DaiFormat SecondDaiFormatFromDaiFormatSets(
    const std::vector<fha::DaiSupportedFormats>& dai_format_sets) {
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
    return fha::DaiFormat{{
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
  if (!ValidateDaiFormat(safe_format_2)) {
    ADD_FAILURE() << "Could not create a second valid DaiFormat";
  }
  return safe_format_2;
}

// From many DaiSupportedFormats, get a DaiFormat that is UNSUPPORTED (but still valid).
fha::DaiFormat UnsupportedDaiFormatFromDaiFormatSets(
    const std::vector<fha::DaiSupportedFormats>& dai_format_sets) {
  auto dai_format = SafeDaiFormatFromDaiFormatSets(dai_format_sets);
  if (dai_format.number_of_channels() > 1) {
    dai_format.number_of_channels() -= 1;
    dai_format.channels_to_use_bitmask() = (1ull << dai_format.number_of_channels()) - 1ull;
  } else if (dai_format.frame_rate() > kMinSupportedDaiFrameRate) {
    dai_format.frame_rate() -= 1;
  } else if (dai_format.bits_per_slot() > 1) {
    dai_format.bits_per_slot() -= 1;
  } else if (dai_format.bits_per_sample() > 1) {
    dai_format.bits_per_sample() -= 1;
  } else {
    ADD_FAILURE() << "No unsupported DaiFormat found for these format_sets";
    return {{}};
  }
  return dai_format;
}

///////////////////////////////
// RingBuffer-related functions
//
// From many fad::PcmFormatSet, get a fuchsia_audio::Format.
fuchsia_audio::Format SafeRingBufferFormatFromRingBufferFormatSets(
    const std::vector<fad::PcmFormatSet>& ring_buffer_format_sets) {
  return {{
      .sample_type = ring_buffer_format_sets.front().sample_types()->front(),
      .channel_count = ring_buffer_format_sets.front().channel_sets()->front().attributes()->size(),
      .frames_per_second = ring_buffer_format_sets.front().frame_rates()->front(),
  }};
}

// From many fad::PcmFormatSet, get a DIFFERENT fuchsia_audio::Format.
fuchsia_audio::Format SecondRingBufferFormatFromRingBufferFormatSets(
    const std::vector<fad::PcmFormatSet>& ring_buffer_format_sets) {
  auto safe_format = SafeRingBufferFormatFromRingBufferFormatSets(ring_buffer_format_sets);
  auto& first_format_set = ring_buffer_format_sets.front();
  if (first_format_set.channel_sets()->size() > 1) {
    safe_format.channel_count() = first_format_set.channel_sets()->rbegin()->attributes()->size();
  } else if (first_format_set.sample_types()->size() > 1) {
    safe_format.sample_type() = *first_format_set.sample_types()->rbegin();
  } else if (first_format_set.frame_rates()->size() > 1) {
    safe_format.frames_per_second() = *first_format_set.frame_rates()->rbegin();
  } else {
    ADD_FAILURE() << "SecondRingBufferFormatFromRingBufferFormatSets: Only one format is possible";
    return {};
  }
  return safe_format;
}

// From a multi-element collection, each with many fad::PcmFormatSet, get a fa::Format.
fuchsia_audio::Format SafeRingBufferFormatFromElementRingBufferFormatSets(
    ElementId element_id,
    const std::vector<fad::ElementRingBufferFormatSet>& element_ring_buffer_format_sets) {
  std::vector<fuchsia_audio::Format> ring_buffer_format_sets;
  for (const auto& element_entry : element_ring_buffer_format_sets) {
    if (element_entry.element_id() && *element_entry.element_id() == element_id) {
      return SafeRingBufferFormatFromRingBufferFormatSets(*element_entry.format_sets());
    }
  }
  ADD_FAILURE()
      << "SafeRingBufferFormatFromElementRingBufferFormatSets: No element_ring_buffer_format_sets entry found with specified element_id "
      << element_id;
  return {};
}

// From a multi-element collection, each with many fad::PcmFormatSet, get a DIFFERENT fa::Format.
fuchsia_audio::Format SecondRingBufferFormatFromElementRingBufferFormatSets(
    ElementId element_id,
    const std::vector<fad::ElementRingBufferFormatSet>& element_ring_buffer_format_sets) {
  std::vector<fuchsia_audio::Format> ring_buffer_format_sets;
  for (const auto& element_entry : element_ring_buffer_format_sets) {
    if (element_entry.element_id() && *element_entry.element_id() == element_id) {
      return SecondRingBufferFormatFromRingBufferFormatSets(*element_entry.format_sets());
    }
  }
  ADD_FAILURE()
      << "SecondRingBufferFormatFromElementRingBufferFormatSets: No element_ring_buffer_format_sets entry found with specified element_id "
      << element_id;
  return {};
}

// From many SupportedFormats, get a Format.
fha::Format SafeDriverRingBufferFormatFromDriverRingBufferFormatSets(
    const std::vector<fha::SupportedFormats>& driver_ring_buffer_format_sets) {
  auto first_format_set = *driver_ring_buffer_format_sets.front().pcm_supported_formats();
  fha::Format ring_buffer_format{{
      .pcm_format = fha::PcmFormat{{
          .number_of_channels =
              static_cast<uint8_t>(first_format_set.channel_sets()->front().attributes()->size()),
          .sample_format = first_format_set.sample_formats()->front(),
          .bytes_per_sample = first_format_set.bytes_per_sample()->front(),
          .valid_bits_per_sample = first_format_set.valid_bits_per_sample()->front(),
          .frame_rate = first_format_set.frame_rates()->front(),
      }},
  }};

  if (!ValidateRingBufferFormat(ring_buffer_format)) {
    ADD_FAILURE() << "first entries did not create a valid DaiFormat";
    return {};
  }
  return ring_buffer_format;
}

// From many SupportedFormats, get a DIFFERENT Format.
fha::Format SecondDriverRingBufferFormatFromDriverRingBufferFormatSets(
    const std::vector<fha::SupportedFormats>& driver_ring_buffer_format_sets) {
  auto safe_format =
      SafeDriverRingBufferFormatFromDriverRingBufferFormatSets(driver_ring_buffer_format_sets);
  auto driver_rb_format_set = *driver_ring_buffer_format_sets.begin()->pcm_supported_formats();
  if (safe_format.pcm_format().has_value()) {
    if (driver_rb_format_set.channel_sets()->size() > 1) {
      safe_format.pcm_format()->number_of_channels() =
          static_cast<uint8_t>(driver_rb_format_set.channel_sets()->rbegin()->attributes()->size());
      return safe_format;
    }
    if (driver_rb_format_set.sample_formats()->size() > 1) {
      safe_format.pcm_format()->sample_format() = *driver_rb_format_set.sample_formats()->rbegin();
      return safe_format;
    }
    if (driver_rb_format_set.bytes_per_sample()->size() > 1) {
      safe_format.pcm_format()->bytes_per_sample() =
          *driver_rb_format_set.bytes_per_sample()->rbegin();
      return safe_format;
    }
    if (driver_rb_format_set.frame_rates()->size() > 1) {
      safe_format.pcm_format()->frame_rate() = *driver_rb_format_set.frame_rates()->rbegin();
      return safe_format;
    }
  }
  ADD_FAILURE()
      << "SecondDriverRingBufferFormatFromDriverRingBufferFormatSets: Only one format is possible";
  return {};
}

// From a multi-element collection, each with many SupportedFormats, get a Format.
fha::Format SafeDriverRingBufferFormatFromElementDriverRingBufferFormatSets(
    ElementId element_id,
    const std::vector<std::pair<ElementId, std::vector<fha::SupportedFormats>>>&
        element_driver_ring_buffer_format_sets) {
  for (const auto& element_entry : element_driver_ring_buffer_format_sets) {
    if (element_entry.first == element_id) {
      return SafeDriverRingBufferFormatFromDriverRingBufferFormatSets(element_entry.second);
    }
  }
  ADD_FAILURE()
      << "SafeDriverRingBufferFormatFromElementDriverRingBufferFormatSets: No element_driver_ring_buffer_format_sets entry found with specified element_id "
      << element_id;
  return {};
}

// From a multi-element collection, each with many SupportedFormats, get ANOTHER Format.
fha::Format SecondDriverRingBufferFormatFromElementDriverRingBufferFormatSets(
    ElementId element_id,
    const std::vector<std::pair<ElementId, std::vector<fha::SupportedFormats>>>&
        element_driver_ring_buffer_format_sets) {
  for (const auto& element_entry : element_driver_ring_buffer_format_sets) {
    if (element_entry.first == element_id) {
      return SecondDriverRingBufferFormatFromDriverRingBufferFormatSets(element_entry.second);
    }
  }
  ADD_FAILURE()
      << "SecondDriverRingBufferFormatFromElementDriverRingBufferFormatSets: No element_driver_ring_buffer_format_sets entry found with specified element_id "
      << element_id;
  return {};
}

}  // namespace media_audio
