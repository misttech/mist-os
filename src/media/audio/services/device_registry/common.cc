// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/common.h"

#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>

#include <vector>

#include "src/media/audio/services/device_registry/basic_types.h"
#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/validate.h"

namespace media_audio {

bool DaiFormatIsSupported(
    ElementId element_id,
    const std::vector<fuchsia_audio_device::ElementDaiFormatSet>& element_dai_format_sets,
    const fuchsia_hardware_audio::DaiFormat& format) {
  std::optional<std::vector<fuchsia_hardware_audio::DaiSupportedFormats>> dai_format_sets;
  for (const auto& element_sets_entry : element_dai_format_sets) {
    if (element_sets_entry.element_id().has_value() &&
        *element_sets_entry.element_id() == element_id) {
      if (!element_sets_entry.format_sets().has_value()) {
        return false;
      }
      dai_format_sets = *element_sets_entry.format_sets();
      break;
    }
  }

  if (!dai_format_sets.has_value()) {
    return false;
  }

  for (const auto& dai_format_set : *dai_format_sets) {
    bool match = false;
    for (auto channel_count : dai_format_set.number_of_channels()) {
      if (channel_count == format.number_of_channels()) {
        match = true;
        break;
      }
    }
    if (!match) {
      continue;
    }

    match = false;
    for (const auto& sample_format : dai_format_set.sample_formats()) {
      if (sample_format == format.sample_format()) {
        match = true;
        break;
      }
    }
    if (!match) {
      continue;
    }

    match = false;
    for (const auto& frame_format : dai_format_set.frame_formats()) {
      if (frame_format == format.frame_format()) {
        match = true;
        break;
      }
    }
    if (!match) {
      continue;
    }

    match = false;
    for (const auto& rate : dai_format_set.frame_rates()) {
      if (rate == format.frame_rate()) {
        match = true;
        break;
      }
    }
    if (!match) {
      continue;
    }

    match = false;
    for (const auto& bits : dai_format_set.bits_per_slot()) {
      if (bits == format.bits_per_slot()) {
        match = true;
        break;
      }
    }
    if (!match) {
      continue;
    }

    match = false;
    for (const auto& bits : dai_format_set.bits_per_sample()) {
      if (bits == format.bits_per_sample()) {
        match = true;
        break;
      }
    }
    if (!match) {
      continue;
    }
    // This DaiFormatSet survived with a match on all aspects.
    return true;
  }
  // None of the DaiFormatSets survived through all of the aspects.
  return false;
}

bool RingBufferFormatIsSupported(
    ElementId element_id,
    const std::vector<fuchsia_audio_device::ElementRingBufferFormatSet>&
        element_ring_buffer_format_sets,
    const fuchsia_hardware_audio::Format& format) {
  if (!ValidateRingBufferFormat(format)) {
    return false;
  }
  std::optional<std::vector<fuchsia_audio_device::PcmFormatSet>> ring_buffer_format_sets;
  for (const auto& element_sets_entry : element_ring_buffer_format_sets) {
    if (element_sets_entry.element_id().has_value() &&
        *element_sets_entry.element_id() == element_id) {
      if (!element_sets_entry.format_sets().has_value()) {
        return false;
      }
      ring_buffer_format_sets = *element_sets_entry.format_sets();
      break;
    }
  }
  if (!ring_buffer_format_sets.has_value()) {
    return false;
  }

  for (const auto& ring_buffer_format_set : *ring_buffer_format_sets) {
    bool match = false;
    if (!ring_buffer_format_set.sample_types()) {
      return false;
    }
    for (auto sample_type : *ring_buffer_format_set.sample_types()) {
      switch (sample_type) {
        case fuchsia_audio::SampleType::kUint8:
          if (format.pcm_format()->sample_format() ==
                  fuchsia_hardware_audio::SampleFormat::kPcmUnsigned &&
              format.pcm_format()->bytes_per_sample() == 1) {
            match = true;
          }
          break;
        case fuchsia_audio::SampleType::kInt16:
          if (format.pcm_format()->sample_format() ==
                  fuchsia_hardware_audio::SampleFormat::kPcmSigned &&
              format.pcm_format()->bytes_per_sample() == 2) {
            match = true;
          }
          break;
        case fuchsia_audio::SampleType::kInt32:
          if (format.pcm_format()->sample_format() ==
                  fuchsia_hardware_audio::SampleFormat::kPcmSigned &&
              format.pcm_format()->bytes_per_sample() == 4) {
            match = true;
          }
          break;
        case fuchsia_audio::SampleType::kFloat32:
          if (format.pcm_format()->sample_format() ==
                  fuchsia_hardware_audio::SampleFormat::kPcmFloat &&
              format.pcm_format()->bytes_per_sample() == 4) {
            match = true;
          }
          break;
        case fuchsia_audio::SampleType::kFloat64:
          if (format.pcm_format()->sample_format() ==
                  fuchsia_hardware_audio::SampleFormat::kPcmFloat &&
              format.pcm_format()->bytes_per_sample() == 8) {
            match = true;
          }
          break;
        default:
          return false;
      }
    }
    if (!match) {  // No match for this RingBufferFormatSet - try the next one.
      continue;
    }

    match = false;
    if (!ring_buffer_format_set.channel_sets().has_value()) {
      return false;
    }
    for (auto channel_set : *ring_buffer_format_set.channel_sets()) {
      if (channel_set.attributes().has_value() &&
          channel_set.attributes()->size() == format.pcm_format()->number_of_channels()) {
        match = true;
        break;
      }
    }
    if (!match) {  // No match for this RingBufferFormatSet - try the next one.
      continue;
    }

    match = false;
    if (!ring_buffer_format_set.frame_rates().has_value()) {
      return false;
    }
    for (auto frame_rate : *ring_buffer_format_set.frame_rates()) {
      if (frame_rate == format.pcm_format()->frame_rate()) {
        match = true;
        break;
      }
    }
    if (!match) {  // No match for this RingBufferFormatSet - try the next one.
      continue;
    }

    // This RingFormatSet survived with a match on all aspects.
    return true;
  }

  // None of the RingBufferFormatSets survived through all of the aspects.
  return false;
}

}  // namespace media_audio
