// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/validate.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <zircon/errors.h>

#include <cmath>
#include <cstdint>
#include <string>
#include <unordered_set>

#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/signal_processing_utils.h"

namespace media_audio {

// Frame rates must be listed in ascending order, but some drivers don't do this.
// TODO(https://fxbug.dev/42068180): once this is fixed, clean out this workaround.
inline constexpr bool kStrictFrameRateOrdering = true;

namespace {

/////////////////////////////////////////////////////
// Utility functions
// In the enclosed vector<SampleFormat>, how many are 'format_to_match'?
size_t CountFormatMatches(const std::vector<fuchsia_hardware_audio::SampleFormat>& sample_formats,
                          fuchsia_hardware_audio::SampleFormat format_to_match) {
  return std::count_if(sample_formats.begin(), sample_formats.end(),
                       [format_to_match](const auto& rb_sample_format) {
                         return rb_sample_format == format_to_match;
                       });
}

// In the enclosed vector<ChannelSet>, how many num_channels equal 'channel_count_to_match'?
size_t CountChannelMatches(const std::vector<fuchsia_hardware_audio::ChannelSet>& channel_sets,
                           size_t channel_count_to_match) {
  return std::count_if(
      channel_sets.begin(), channel_sets.end(),
      [channel_count_to_match](const fuchsia_hardware_audio::ChannelSet& channel_set) {
        return channel_set.attributes()->size() == channel_count_to_match;
      });
}

// In the enclosed vector<uint8_t>, how many values equal 'uchar_to_match'?
size_t CountUcharMatches(const std::vector<uint8_t>& uchars, size_t uchar_to_match) {
  return std::count_if(uchars.begin(), uchars.end(),
                       [uchar_to_match](const auto& uchar) { return uchar == uchar_to_match; });
}

}  // namespace

bool ClientIsValidForDeviceType(const fuchsia_audio_device::DeviceType& device_type,
                                const fuchsia_audio_device::DriverClient& driver_client) {
  switch (driver_client.Which()) {
    case fuchsia_audio_device::DriverClient::Tag::kCodec:
      return (device_type == fuchsia_audio_device::DeviceType::kCodec);
    case fuchsia_audio_device::DriverClient::Tag::kComposite:
      return (device_type == fuchsia_audio_device::DeviceType::kComposite);
    case fuchsia_audio_device::DriverClient::Tag::kDai:
      return (device_type == fuchsia_audio_device::DeviceType::kDai);
    case fuchsia_audio_device::DriverClient::Tag::kStreamConfig:
      return (device_type == fuchsia_audio_device::DeviceType::kInput ||
              device_type == fuchsia_audio_device::DeviceType::kOutput);
    default:
      return false;
  }
}

// Translate from fuchsia_hardware_audio::SupportedFormats to fuchsia_audio_device::PcmFormatSet.
std::vector<fuchsia_audio_device::PcmFormatSet> TranslateRingBufferFormatSets(
    const std::vector<fuchsia_hardware_audio::SupportedFormats>& ring_buffer_format_sets) {
  // translated_ring_buffer_format_sets is more complex to copy, since fuchsia_audio_device defines
  // its tables from scratch instead of reusing types from fuchsia_hardware_audio. We build from the
  // inside-out: populating attributes then channel_sets then translated_ring_buffer_format_sets.
  std::vector<fuchsia_audio_device::PcmFormatSet> translated_ring_buffer_format_sets;
  for (auto& ring_buffer_format_set : ring_buffer_format_sets) {
    auto& pcm_formats = *ring_buffer_format_set.pcm_supported_formats();

    const uint32_t max_format_rate =
        *std::max_element(pcm_formats.frame_rates()->begin(), pcm_formats.frame_rates()->end());

    // Construct channel_sets
    std::vector<fuchsia_audio_device::ChannelSet> channel_sets;
    for (const auto& chan_set : *pcm_formats.channel_sets()) {
      std::vector<fuchsia_audio_device::ChannelAttributes> attributes;
      for (const auto& attribs : *chan_set.attributes()) {
        std::optional<uint32_t> max_channel_frequency;
        if (attribs.max_frequency()) {
          max_channel_frequency = std::min(*attribs.max_frequency(), max_format_rate / 2);
        }
        attributes.push_back({{
            .min_frequency = attribs.min_frequency(),
            .max_frequency = max_channel_frequency,
        }});
      }
      channel_sets.push_back({{.attributes = attributes}});
    }
    if (channel_sets.empty()) {
      FX_LOGS(WARNING) << "Could not translate a format set - channel_sets was empty";
      continue;
    }

    // Construct our sample_types by intersecting vectors received from the device.
    // fuchsia_audio::SampleType defines a sparse set of types, so we populate the vector
    // in a bespoke manner (first unsigned, then signed, then float).
    std::vector<fuchsia_audio::SampleType> sample_types;
    if (CountFormatMatches(*pcm_formats.sample_formats(),
                           fuchsia_hardware_audio::SampleFormat::kPcmUnsigned) > 0 &&
        CountUcharMatches(*pcm_formats.bytes_per_sample(), 1) > 0) {
      sample_types.push_back(fuchsia_audio::SampleType::kUint8);
    }
    if (CountFormatMatches(*pcm_formats.sample_formats(),
                           fuchsia_hardware_audio::SampleFormat::kPcmSigned) > 0) {
      if (CountUcharMatches(*pcm_formats.bytes_per_sample(), 2) > 0) {
        sample_types.push_back(fuchsia_audio::SampleType::kInt16);
      }
      if (CountUcharMatches(*pcm_formats.bytes_per_sample(), 4) > 0) {
        sample_types.push_back(fuchsia_audio::SampleType::kInt32);
      }
    }
    if (CountFormatMatches(*pcm_formats.sample_formats(),
                           fuchsia_hardware_audio::SampleFormat::kPcmFloat) > 0 &&
        CountUcharMatches(*pcm_formats.bytes_per_sample(), 4) > 0) {
      if (CountUcharMatches(*pcm_formats.bytes_per_sample(), 4) > 0) {
        sample_types.push_back(fuchsia_audio::SampleType::kFloat32);
      }
      if (CountUcharMatches(*pcm_formats.bytes_per_sample(), 8) > 0) {
        sample_types.push_back(fuchsia_audio::SampleType::kFloat64);
      }
    }
    if (sample_types.empty()) {
      FX_LOGS(WARNING) << "Could not translate a format set - sample_types was empty";
      continue;
    }

    if (pcm_formats.frame_rates()->empty()) {
      FX_LOGS(WARNING) << "Could not translate a format set - frame_rates was empty";
      continue;
    }
    // Make a copy of the frame_rates result, so we can sort it.
    std::vector<uint32_t> frame_rates = *pcm_formats.frame_rates();
    std::sort(frame_rates.begin(), frame_rates.end());

    fuchsia_audio_device::PcmFormatSet pcm_format_set = {{
        .channel_sets = channel_sets,
        .sample_types = sample_types,
        .frame_rates = frame_rates,
    }};
    translated_ring_buffer_format_sets.emplace_back(pcm_format_set);
  }
  return translated_ring_buffer_format_sets;
}

zx_status_t ValidateStreamProperties(
    const fuchsia_hardware_audio::StreamProperties& stream_props,
    std::optional<const fuchsia_hardware_audio::GainState> gain_state,
    std::optional<const fuchsia_hardware_audio::PlugState> plug_state) {
  ADR_LOG(kLogDeviceMethods);
  LogStreamProperties(stream_props);

  if (!stream_props.is_input() || !stream_props.min_gain_db() || !stream_props.max_gain_db() ||
      !stream_props.gain_step_db() || !stream_props.plug_detect_capabilities() ||
      !stream_props.clock_domain()) {
    FX_LOGS(WARNING) << "Incomplete StreamConfig/GetProperties response";
    return ZX_ERR_INVALID_ARGS;
  }

  if ((stream_props.manufacturer().has_value() && stream_props.manufacturer()->empty()) ||
      (stream_props.product().has_value() && stream_props.product()->empty())) {
    FX_LOGS(WARNING) << __func__
                     << ": manufacturer and product, if present, must not be empty strings";
    return ZX_ERR_INVALID_ARGS;
  }

  // Eliminate NaN or infinity values
  if (!std::isfinite(*stream_props.min_gain_db())) {
    FX_LOGS(WARNING) << "Reported min_gain_db is NaN or infinity";
    return ZX_ERR_INVALID_ARGS;
  }
  if (!std::isfinite(*stream_props.max_gain_db())) {
    FX_LOGS(WARNING) << "Reported max_gain_db is NaN or infinity";
    return ZX_ERR_INVALID_ARGS;
  }
  if (!std::isfinite(*stream_props.gain_step_db())) {
    FX_LOGS(WARNING) << "Reported gain_step_db is NaN or infinity";
    return ZX_ERR_INVALID_ARGS;
  }

  if (*stream_props.min_gain_db() > *stream_props.max_gain_db()) {
    FX_LOGS(WARNING) << "GetProperties: min_gain_db cannot exceed max_gain_db: "
                     << *stream_props.min_gain_db() << "," << *stream_props.max_gain_db();
    return ZX_ERR_INVALID_ARGS;
  }
  if (*stream_props.gain_step_db() > *stream_props.max_gain_db() - *stream_props.min_gain_db()) {
    FX_LOGS(WARNING) << "GetProperties: gain_step_db cannot exceed max_gain_db-min_gain_db: "
                     << *stream_props.gain_step_db() << ","
                     << *stream_props.max_gain_db() - *stream_props.min_gain_db();
    return ZX_ERR_INVALID_ARGS;
  }
  if (*stream_props.gain_step_db() < 0.0f) {
    FX_LOGS(WARNING) << "GetProperties: gain_step_db (" << *stream_props.gain_step_db()
                     << ") cannot be negative";
    return ZX_ERR_INVALID_ARGS;
  }

  // If we already have this device's GainState, double-check against that.
  if (gain_state) {
    if (*gain_state->gain_db() < *stream_props.min_gain_db() ||
        *gain_state->gain_db() > *stream_props.max_gain_db()) {
      FX_LOGS(WARNING) << "Gain range reported by GetProperties does not include current gain_db: "
                       << *gain_state->gain_db();
      return ZX_ERR_INVALID_ARGS;
    }

    // Device can't mute (or doesn't say it can), but says it is currently muted...
    if (!stream_props.can_mute().value_or(false) && gain_state->muted().value_or(false)) {
      FX_LOGS(WARNING) << "GetProperties reports can_mute FALSE, but device is muted";
      return ZX_ERR_INVALID_ARGS;
    }
    // Device doesn't have AGC (or doesn't say it does), but says AGC is currently enabled...
    if (!stream_props.can_agc().value_or(false) && gain_state->agc_enabled().value_or(false)) {
      FX_LOGS(WARNING) << "GetProperties reports can_agc FALSE, but AGC is enabled";
      return ZX_ERR_INVALID_ARGS;
    }
  }

  // If we already have this device's PlugState, double-check against that.
  if (plug_state && !(*plug_state->plugged()) &&
      *stream_props.plug_detect_capabilities() ==
          fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired) {
    FX_LOGS(WARNING) << "GetProperties reports HARDWIRED, but StreamConfig reports as UNPLUGGED";
    return ZX_ERR_INVALID_ARGS;
  }

  return ZX_OK;
}

zx_status_t ValidateRingBufferFormatSets(
    const std::vector<fuchsia_hardware_audio::SupportedFormats>& ring_buffer_format_sets) {
  ADR_LOG(kLogDeviceMethods);
  LogRingBufferFormatSets(ring_buffer_format_sets);

  if (ring_buffer_format_sets.empty()) {
    FX_LOGS(WARNING) << "GetRingBufferFormatSets: ring_buffer_format_sets[] is empty";
    return ZX_ERR_INVALID_ARGS;
  }

  for (const auto& rb_format_set : ring_buffer_format_sets) {
    if (!rb_format_set.pcm_supported_formats()) {
      FX_LOGS(WARNING) << "GetSupportedFormats: pcm_supported_formats is absent";
      return ZX_ERR_INVALID_ARGS;
    }
    const auto& pcm_format_set = *rb_format_set.pcm_supported_formats();

    // Frame rates
    if (!pcm_format_set.frame_rates() || pcm_format_set.frame_rates()->empty()) {
      FX_LOGS(WARNING) << "GetSupportedFormats: frame_rates[] is "
                       << (pcm_format_set.frame_rates() ? "empty" : "absent");
      return ZX_ERR_INVALID_ARGS;
    }
    // While testing frame_rates, we can determine max_supported_frame_rate.
    uint32_t prev_frame_rate = 0, max_supported_frame_rate = 0;
    for (const auto& rate : *pcm_format_set.frame_rates()) {
      if (rate < kMinSupportedRingBufferFrameRate || rate > kMaxSupportedRingBufferFrameRate) {
        FX_LOGS(WARNING) << "GetSupportedFormats: frame_rate (" << rate << ") out of range ["
                         << kMinSupportedRingBufferFrameRate << ","
                         << kMaxSupportedRingBufferFrameRate << "] ";
        return ZX_ERR_OUT_OF_RANGE;
      }
      // Checking for "strictly ascending" also eliminates duplicate entries.
      if (rate <= prev_frame_rate) {
        FX_LOGS(WARNING) << "GetSupportedFormats: frame_rate must be in ascending order: "
                         << prev_frame_rate << " was listed before " << rate;
        return ZX_ERR_INVALID_ARGS;
      }
      prev_frame_rate = rate;
      max_supported_frame_rate = std::max(max_supported_frame_rate, rate);
    }

    // Channel sets
    if (!pcm_format_set.channel_sets() || pcm_format_set.channel_sets()->empty()) {
      FX_LOGS(WARNING) << "GetSupportedFormats: channel_sets[] is "
                       << (pcm_format_set.channel_sets() ? "empty" : "absent");
      return ZX_ERR_INVALID_ARGS;
    }
    auto max_allowed_frequency = max_supported_frame_rate / 2;
    for (const fuchsia_hardware_audio::ChannelSet& chan_set : *pcm_format_set.channel_sets()) {
      if (!chan_set.attributes() || chan_set.attributes()->empty()) {
        FX_LOGS(WARNING) << "GetSupportedFormats: ChannelSet.attributes[] is "
                         << (chan_set.attributes() ? "empty" : "absent");
        return ZX_ERR_INVALID_ARGS;
      }
      if (CountChannelMatches(*pcm_format_set.channel_sets(), chan_set.attributes()->size()) > 1) {
        FX_LOGS(WARNING)
            << "GetSupportedFormats: channel-count must be unique across channel_sets: "
            << chan_set.attributes()->size();
        return ZX_ERR_INVALID_ARGS;
      }
      for (const auto& attrib : *chan_set.attributes()) {
        if (attrib.min_frequency()) {
          if (*attrib.min_frequency() > max_allowed_frequency) {
            FX_LOGS(WARNING) << "GetSupportedFormats: ChannelAttributes.min_frequency ("
                             << *attrib.min_frequency() << ") out of range: " << "[0, "
                             << max_allowed_frequency << "]";
            return ZX_ERR_OUT_OF_RANGE;
          }
          if (attrib.max_frequency() && *attrib.min_frequency() > *attrib.max_frequency()) {
            FX_LOGS(WARNING) << "GetSupportedFormats: min_frequency (" << *attrib.min_frequency()
                             << ") cannot exceed max_frequency (" << *attrib.max_frequency() << ")";
            return ZX_ERR_INVALID_ARGS;
          }
        }

        if (attrib.max_frequency()) {
          if (*attrib.max_frequency() > max_allowed_frequency) {
            FX_LOGS(WARNING) << "GetSupportedFormats: ChannelAttrib.max_frequency "
                             << *attrib.max_frequency() << " will be limited to "
                             << max_allowed_frequency;
          }
        }
      }
    }

    // Sample format
    if (!pcm_format_set.sample_formats() || pcm_format_set.sample_formats()->empty()) {
      FX_LOGS(WARNING) << "GetSupportedFormats: sample_formats[] is "
                       << (pcm_format_set.sample_formats() ? "empty" : "absent");
      return ZX_ERR_INVALID_ARGS;
    }
    const auto& rb_sample_formats = *pcm_format_set.sample_formats();
    for (const auto& format : rb_sample_formats) {
      if (CountFormatMatches(rb_sample_formats, format) > 1) {
        FX_LOGS(WARNING) << "GetSupportedFormats: no duplicate SampleFormat values allowed: "
                         << format;
        return ZX_ERR_INVALID_ARGS;
      }
    }

    // Bytes per sample
    if (!pcm_format_set.bytes_per_sample() || pcm_format_set.bytes_per_sample()->empty()) {
      FX_LOGS(WARNING) << "GetSupportedFormats: bytes_per_sample[] is "
                       << (pcm_format_set.bytes_per_sample() ? "empty" : "absent");
      return ZX_ERR_INVALID_ARGS;
    }
    uint8_t prev_bytes_per_sample = 0, max_bytes_per_sample = 0;
    for (const auto& bytes : *pcm_format_set.bytes_per_sample()) {
      if (CountFormatMatches(rb_sample_formats, fuchsia_hardware_audio::SampleFormat::kPcmSigned) &&
          (bytes != 2 && bytes != 4)) {
        FX_LOGS(WARNING) << "GetSupportedFormats: bytes_per_sample ("
                         << static_cast<uint16_t>(bytes)
                         << ") must be 2 or 4 for PCM_SIGNED format";
        return ZX_ERR_INVALID_ARGS;
      }
      if (CountFormatMatches(rb_sample_formats, fuchsia_hardware_audio::SampleFormat::kPcmFloat) &&
          (bytes != 4 && bytes != 8)) {
        FX_LOGS(WARNING) << "GetSupportedFormats: bytes_per_sample ("
                         << static_cast<uint16_t>(bytes) << ") must be 4 or 8 for PCM_FLOAT format";
        return ZX_ERR_INVALID_ARGS;
      }
      if (CountFormatMatches(rb_sample_formats,
                             fuchsia_hardware_audio::SampleFormat::kPcmUnsigned) &&
          bytes != 1) {
        FX_LOGS(WARNING) << "GetSupportedFormats: bytes_per_sample ("
                         << static_cast<uint16_t>(bytes) << ") must be 1 for PCM_UNSIGNED format";
        return ZX_ERR_INVALID_ARGS;
      }
      // Checking for "strictly ascending" also eliminates duplicate entries.
      if (bytes <= prev_bytes_per_sample) {
        FX_LOGS(WARNING) << "GetSupportedFormats: bytes_per_sample must be in ascending order: "
                         << static_cast<uint16_t>(prev_bytes_per_sample) << " was listed before "
                         << static_cast<uint16_t>(bytes);
        return ZX_ERR_INVALID_ARGS;
      }
      prev_bytes_per_sample = bytes;

      max_bytes_per_sample = std::max(max_bytes_per_sample, bytes);
    }

    // Valid bits per sample
    if (!pcm_format_set.valid_bits_per_sample() ||
        pcm_format_set.valid_bits_per_sample()->empty()) {
      FX_LOGS(WARNING) << "GetSupportedFormats: valid_bits_per_sample[] is "
                       << (pcm_format_set.valid_bits_per_sample() ? "empty" : "absent");
      return ZX_ERR_INVALID_ARGS;
    }
    uint8_t prev_valid_bits = 0;
    for (const auto& valid_bits : *pcm_format_set.valid_bits_per_sample()) {
      if (valid_bits == 0 || valid_bits > max_bytes_per_sample * 8) {
        FX_LOGS(WARNING) << "GetSupportedFormats: valid_bits_per_sample ("
                         << static_cast<uint16_t>(valid_bits) << ") out of range [1, "
                         << max_bytes_per_sample * 8 << "]";
        return ZX_ERR_OUT_OF_RANGE;
      }
      // Checking for "strictly ascending" also eliminates duplicate entries.
      if (valid_bits <= prev_valid_bits) {
        FX_LOGS(WARNING)
            << "GetSupportedFormats: valid_bits_per_sample must be in ascending order: "
            << static_cast<uint16_t>(prev_valid_bits) << " was listed before "
            << static_cast<uint16_t>(valid_bits);
        return ZX_ERR_INVALID_ARGS;
      }
      prev_valid_bits = valid_bits;
    }
  }

  return ZX_OK;
}

zx_status_t ValidateCodecProperties(
    const fuchsia_hardware_audio::CodecProperties& codec_props,
    std::optional<const fuchsia_hardware_audio::PlugState> plug_state) {
  ADR_LOG(kLogDeviceMethods);
  LogCodecProperties(codec_props);

  if ((codec_props.manufacturer().has_value() && codec_props.manufacturer()->empty()) ||
      (codec_props.product().has_value() && codec_props.product()->empty())) {
    FX_LOGS(WARNING) << __func__
                     << ": manufacturer and product, if present, must not be empty strings";
    return ZX_ERR_INVALID_ARGS;
  }

  if (!codec_props.plug_detect_capabilities()) {
    FX_LOGS(WARNING) << "Incomplete Codec/GetProperties response";
    return ZX_ERR_INVALID_ARGS;
  }

  // If we already have this device's PlugState, double-check against that.
  if (plug_state && !(*plug_state->plugged()) &&
      *codec_props.plug_detect_capabilities() ==
          fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired) {
    FX_LOGS(WARNING) << "GetProperties reports HARDWIRED, but Codec reports as UNPLUGGED";
    return ZX_ERR_INVALID_ARGS;
  }

  return ZX_OK;
}

zx_status_t ValidateDaiFormatSets(
    const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets) {
  ADR_LOG(kLogDeviceMethods);
  LogDaiFormatSets(dai_format_sets);

  if (dai_format_sets.empty()) {
    FX_LOGS(WARNING) << "GetDaiSupportedFormats: response is empty";
    return ZX_ERR_INVALID_ARGS;
  }

  for (const auto& dai_format_set : dai_format_sets) {
    if (dai_format_set.number_of_channels().empty()) {
      FX_LOGS(WARNING) << "Non-compliant DaiSupportedFormats: empty number_of_channels vector";
      return ZX_ERR_INVALID_ARGS;
    }
    uint32_t previous_chans = 0;
    for (const auto& chans : dai_format_set.number_of_channels()) {
      if (chans <= previous_chans ||
          chans > fuchsia_hardware_audio::kMaxCountDaiSupportedNumberOfChannels) {
        FX_LOGS(WARNING) << "Non-compliant DaiSupportedFormats number_of_channels: " << chans;
        return ZX_ERR_INVALID_ARGS;
      }
      previous_chans = chans;
    }
    if (dai_format_set.sample_formats().empty()) {
      FX_LOGS(WARNING) << "Non-compliant DaiSupportedFormats: empty sample_formats vector";
      return ZX_ERR_INVALID_ARGS;
    }
    for (const auto& dai_sample_format : dai_format_set.sample_formats()) {
      if (std::count(dai_format_set.sample_formats().begin(), dai_format_set.sample_formats().end(),
                     dai_sample_format) > 1) {
        FX_LOGS(WARNING) << "Duplicate DaiSupportedFormats sample_format: " << dai_sample_format;
        return ZX_ERR_INVALID_ARGS;
      }
    }
    if (dai_format_set.frame_formats().empty()) {
      FX_LOGS(WARNING) << "Non-compliant DaiSupportedFormats: empty frame_formats vector";
      return ZX_ERR_INVALID_ARGS;
    }
    for (const auto& dai_frame_format : dai_format_set.frame_formats()) {
      if (std::count(dai_format_set.frame_formats().begin(), dai_format_set.frame_formats().end(),
                     dai_frame_format) > 1) {
        FX_LOGS(WARNING) << "Duplicate DaiSupportedFormats frame_format: " << dai_frame_format;
        return ZX_ERR_INVALID_ARGS;
      }
    }
    if (dai_format_set.frame_rates().empty()) {
      FX_LOGS(WARNING) << "Non-compliant DaiSupportedFormats: empty frame_rates vector";
      return ZX_ERR_INVALID_ARGS;
    }
    uint32_t previous_rate = 0;
    for (const auto& rate : dai_format_set.frame_rates()) {
      if (rate <= previous_rate || rate < kMinSupportedDaiFrameRate ||
          rate > kMaxSupportedDaiFrameRate) {
        FX_LOGS(WARNING) << "Non-compliant DaiSupportedFormats frame_rate: " << rate;
        return ZX_ERR_INVALID_ARGS;
      }
      previous_rate = rate;
    }
    if (dai_format_set.bits_per_slot().empty()) {
      FX_LOGS(WARNING) << "Non-compliant DaiSupportedFormats: empty bits_per_slot vector";
      return ZX_ERR_INVALID_ARGS;
    }
    uint32_t previous_bits_per_slot = 0;
    uint32_t max_bits_per_slot = 0;
    for (const auto& bits : dai_format_set.bits_per_slot()) {
      if (bits <= previous_bits_per_slot || bits == 0 || bits > kMaxSupportedDaiFormatBitsPerSlot) {
        FX_LOGS(WARNING) << "Non-compliant DaiSupportedFormats bits_per_slot: "
                         << static_cast<uint16_t>(bits);
        return ZX_ERR_INVALID_ARGS;
      }
      max_bits_per_slot = std::max<uint32_t>(bits, max_bits_per_slot);
      previous_bits_per_slot = bits;
    }
    if (dai_format_set.bits_per_sample().empty()) {
      FX_LOGS(WARNING) << "Non-compliant DaiSupportedFormats: empty bits_per_sample vector";
      return ZX_ERR_INVALID_ARGS;
    }
    uint32_t previous_bits_per_sample = 0;
    for (const auto& bits : dai_format_set.bits_per_sample()) {
      if (bits <= previous_bits_per_sample || bits > max_bits_per_slot || bits == 0) {
        FX_LOGS(WARNING) << "Non-compliant DaiSupportedFormats bits_per_sample: "
                         << static_cast<uint16_t>(bits);
        return ZX_ERR_INVALID_ARGS;
      }
      previous_bits_per_sample = bits;
    }
  }

  return ZX_OK;
}

zx_status_t ValidateDaiFormat(const fuchsia_hardware_audio::DaiFormat& dai_format) {
  ADR_LOG(kLogDeviceMethods);
  LogDaiFormat(dai_format);

  if (dai_format.number_of_channels() == 0 ||
      dai_format.number_of_channels() >
          fuchsia_hardware_audio::kMaxCountDaiSupportedNumberOfChannels) {
    FX_LOGS(WARNING) << "Non-compliant DaiFormat number_of_channels: "
                     << dai_format.number_of_channels();
    return ZX_ERR_INVALID_ARGS;
  }

  if (dai_format.channels_to_use_bitmask() == 0) {
    FX_LOGS(WARNING) << "Non-compliant DaiFormat channels_to_use_bitmask: 0";
    return ZX_ERR_INVALID_ARGS;
  }
  if (dai_format.number_of_channels() < 64 &&
      (dai_format.channels_to_use_bitmask() >> dai_format.number_of_channels()) > 0) {
    FX_LOGS(WARNING) << "Non-compliant DaiFormat channels_to_use_bitmask: 0x" << std::hex
                     << dai_format.channels_to_use_bitmask() << " is too large for " << std::dec
                     << dai_format.number_of_channels() << " channels";
    return ZX_ERR_INVALID_ARGS;
  }

  switch (dai_format.sample_format()) {
    case fuchsia_hardware_audio::DaiSampleFormat::kPdm:
    case fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned:
    case fuchsia_hardware_audio::DaiSampleFormat::kPcmUnsigned:
    case fuchsia_hardware_audio::DaiSampleFormat::kPcmFloat:
      break;
    default:
      FX_LOGS(WARNING) << "Non-compliant DaiFormat sample_format: UNKNOWN enum";
      return ZX_ERR_INVALID_ARGS;
  }

  if (!dai_format.frame_format().frame_format_custom().has_value() &&
      !dai_format.frame_format().frame_format_standard().has_value()) {
    FX_LOGS(WARNING) << "Non-compliant DaiFormat frame_format: UNKNOWN union enum";
    return ZX_ERR_INVALID_ARGS;
  }
  switch (dai_format.frame_format().Which()) {
    case fuchsia_hardware_audio::DaiFrameFormat::Tag::kFrameFormatStandard:
    case fuchsia_hardware_audio::DaiFrameFormat::Tag::kFrameFormatCustom:
      break;
    default:
      FX_LOGS(WARNING) << "Non-compliant DaiFormat frame_format: UNKNOWN union tag";
      return ZX_ERR_INVALID_ARGS;
  }

  if (dai_format.frame_rate() < kMinSupportedDaiFrameRate ||
      dai_format.frame_rate() > kMaxSupportedDaiFrameRate) {
    FX_LOGS(WARNING) << "Non-compliant DaiFormat frame_rate: " << dai_format.frame_rate();
    return ZX_ERR_INVALID_ARGS;
  }

  if (dai_format.bits_per_slot() == 0 ||
      dai_format.bits_per_slot() > kMaxSupportedDaiFormatBitsPerSlot) {
    FX_LOGS(WARNING) << "Non-compliant DaiFormat bits_per_slot: "
                     << static_cast<uint16_t>(dai_format.bits_per_slot());
    return ZX_ERR_INVALID_ARGS;
  }

  if (dai_format.bits_per_sample() == 0 ||
      dai_format.bits_per_sample() > dai_format.bits_per_slot()) {
    FX_LOGS(WARNING) << "Non-compliant DaiFormat bits_per_sample: "
                     << static_cast<uint16_t>(dai_format.bits_per_sample());
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t ValidateCodecFormatInfo(const fuchsia_hardware_audio::CodecFormatInfo& format_info) {
  ADR_LOG(kLogDeviceMethods);
  LogCodecFormatInfo(format_info);

  if (format_info.external_delay() && *format_info.external_delay() < 0) {
    FX_LOGS(WARNING) << "Invalid Codec::SetDaiFormat response - external_delay cannot be negative";
    return ZX_ERR_INVALID_ARGS;
  }
  if (format_info.turn_on_delay() && *format_info.turn_on_delay() < 0) {
    FX_LOGS(WARNING) << "Invalid Codec::SetDaiFormat response - turn_on_delay cannot be negative";
    return ZX_ERR_INVALID_ARGS;
  }
  if (format_info.turn_off_delay() && *format_info.turn_off_delay() < 0) {
    FX_LOGS(WARNING) << "Invalid Codec::SetDaiFormat response - turn_off_delay cannot be negative";
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t ValidateCompositeProperties(
    const fuchsia_hardware_audio::CompositeProperties& composite_props) {
  LogCompositeProperties(composite_props);

  if (!composite_props.clock_domain()) {
    FX_LOGS(WARNING) << "Incomplete Composite/GetProperties response";
    return ZX_ERR_INVALID_ARGS;
  }

  if ((composite_props.manufacturer().has_value() && composite_props.manufacturer()->empty()) ||
      (composite_props.product().has_value() && composite_props.product()->empty())) {
    FX_LOGS(WARNING) << __func__
                     << ": manufacturer and product, if present, must not be empty strings";
    return ZX_ERR_INVALID_ARGS;
  }

  return ZX_OK;
}

zx_status_t ValidateGainState(
    const fuchsia_hardware_audio::GainState& gain_state,
    std::optional<const fuchsia_hardware_audio::StreamProperties> stream_props) {
  ADR_LOG(kLogDeviceMethods);
  LogGainState(gain_state);

  if (!gain_state.gain_db()) {
    FX_LOGS(WARNING) << "Incomplete StreamConfig/WatchGainState response";
    return ZX_ERR_INVALID_ARGS;
  }

  // Eliminate NaN or infinity values
  if (!std::isfinite(*gain_state.gain_db())) {
    FX_LOGS(WARNING) << "Reported gain_db is NaN or infinity";
    return ZX_ERR_INVALID_ARGS;
  }

  // If we already have this device's GainCapabilities, double-check against those.
  if (stream_props) {
    if (*gain_state.gain_db() < *stream_props->min_gain_db() ||
        *gain_state.gain_db() > *stream_props->max_gain_db()) {
      FX_LOGS(WARNING) << "Reported gain_db is out of range: " << *gain_state.gain_db();
      return ZX_ERR_OUT_OF_RANGE;
    }
    // Device reports it can't mute (or doesn't say it can), then DOES say that it is muted....
    if (!stream_props->can_mute().value_or(false) && gain_state.muted().value_or(false)) {
      FX_LOGS(WARNING) << "Reported 'muted' state (TRUE) is unsupported";
      return ZX_ERR_INVALID_ARGS;
    }
    // Device reports it can't AGC (or doesn't say it can), then DOES say that AGC is enabled....
    if (!stream_props->can_agc().value_or(false) && gain_state.agc_enabled().value_or(false)) {
      FX_LOGS(WARNING) << "Reported 'agc_enabled' state (TRUE) is unsupported";
      return ZX_ERR_INVALID_ARGS;
    }
  }

  return ZX_OK;
}

zx_status_t ValidatePlugState(
    const fuchsia_hardware_audio::PlugState& plug_state,
    std::optional<fuchsia_hardware_audio::PlugDetectCapabilities> plug_detect_capabilities) {
  ADR_LOG(kLogDeviceMethods);
  LogPlugState(plug_state);

  if (!plug_state.plugged() || !plug_state.plug_state_time()) {
    FX_LOGS(WARNING) << "Incomplete StreamConfig/WatchPlugState response: required field missing";
    return ZX_ERR_INVALID_ARGS;
  }

  int64_t now = zx::clock::get_monotonic().get();
  if (*plug_state.plug_state_time() > now) {
    FX_LOGS(WARNING) << "Reported plug_time is in the future: " << *plug_state.plug_state_time();
    return ZX_ERR_INVALID_ARGS;
  }

  // If we already have this device's PlugDetectCapabilities, double-check against those.
  if (plug_detect_capabilities) {
    if (*plug_detect_capabilities == fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired &&
        !plug_state.plugged().value_or(true)) {
      FX_LOGS(WARNING) << "Reported 'plug_state' (UNPLUGGED) is unsupported (HARDWIRED)";
      return ZX_ERR_INVALID_ARGS;
    }
  }

  return ZX_OK;
}

// Validate only DeviceInfo-specific aspects. For example, don't re-validate format correctness.
bool ValidateDeviceInfo(const fuchsia_audio_device::Info& device_info) {
  ADR_LOG(kLogDeviceMethods);
  LogDeviceInfo(device_info);

  // Validate top-level required members.
  if (!device_info.token_id().has_value() || !device_info.device_type().has_value() ||
      !device_info.device_name().has_value() || device_info.device_name()->empty()) {
    FX_LOGS(WARNING) << __func__ << ": incomplete DeviceInfo instance";
    return false;
  }
  // These strings must not be empty, if present.
  if ((device_info.manufacturer().has_value() && device_info.manufacturer()->empty()) ||
      (device_info.product().has_value() && device_info.product()->empty())) {
    FX_LOGS(WARNING) << __func__
                     << ": manufacturer and product, if present, must not be empty strings";
    return false;
  }
  // These vectors must not be empty, if present.
  if ((device_info.signal_processing_elements().has_value() &&
       device_info.signal_processing_elements()->empty()) ||
      (device_info.signal_processing_topologies().has_value() &&
       device_info.signal_processing_topologies()->empty())) {
    FX_LOGS(WARNING)
        << __func__
        << ": signal_processing elements/topologies, if present, must have at least one entry";
    return false;
  }
  switch (*device_info.device_type()) {
    case fuchsia_audio_device::DeviceType::kCodec:
      if (!device_info.dai_format_sets().has_value() || device_info.dai_format_sets()->empty() ||
          !device_info.plug_detect_caps().has_value()) {
        FX_LOGS(WARNING) << __func__ << ": incomplete DeviceInfo instance";
        return false;
      }
      if (device_info.ring_buffer_format_sets().has_value() ||
          device_info.gain_caps().has_value() || device_info.clock_domain().has_value()) {
        FX_LOGS(WARNING) << __func__ << ": invalid DeviceInfo fields are populated";
        return false;
      }
      break;
    case fuchsia_audio_device::DeviceType::kComposite:
      if (!device_info.clock_domain().has_value() ||
          !device_info.signal_processing_elements().has_value() ||
          !device_info.signal_processing_topologies().has_value()) {
        FX_LOGS(WARNING) << __func__ << ": incomplete DeviceInfo instance";
        return false;
      }
      if (device_info.is_input().has_value() || device_info.gain_caps().has_value() ||
          device_info.plug_detect_caps().has_value()) {
        FX_LOGS(WARNING) << __func__ << ": invalid DeviceInfo fields are populated";
        return false;
      }
      break;
    case fuchsia_audio_device::DeviceType::kInput:
    case fuchsia_audio_device::DeviceType::kOutput:
      if (!device_info.is_input().has_value() || !device_info.gain_caps().has_value() ||
          !device_info.ring_buffer_format_sets().has_value() ||
          device_info.ring_buffer_format_sets()->empty() ||
          !device_info.plug_detect_caps().has_value() || !device_info.clock_domain().has_value()) {
        FX_LOGS(WARNING) << __func__ << ": incomplete DeviceInfo instance";
        return false;
      }
      if (device_info.dai_format_sets().has_value()) {
        FX_LOGS(WARNING) << __func__ << ": invalid DeviceInfo fields are populated";
        return false;
      }
      break;
    case fuchsia_audio_device::DeviceType::kDai:
    default:
      FX_LOGS(WARNING) << __func__ << ": unsupported DeviceType: " << device_info.device_type();
      return false;
  }

  return true;
}

zx_status_t ValidateRingBufferProperties(
    const fuchsia_hardware_audio::RingBufferProperties& rb_props) {
  ADR_LOG(kLogDeviceMethods);
  LogRingBufferProperties(rb_props);

  if (!rb_props.needs_cache_flush_or_invalidate()) {
    FX_LOGS(WARNING) << "Reported RingBufferProperties.needs_cache_flush_or_invalidate is missing";
    return ZX_ERR_INVALID_ARGS;
  }
  if (rb_props.turn_on_delay() && *rb_props.turn_on_delay() < 0) {
    FX_LOGS(WARNING) << "Reported RingBufferProperties.turn_on_delay is negative";
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (!rb_props.driver_transfer_bytes()) {
    FX_LOGS(WARNING) << "Reported RingBufferProperties.driver_transfer_bytes is missing";
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t ValidateRingBufferFormat(const fuchsia_hardware_audio::Format& ring_buffer_format) {
  ADR_LOG(kLogDeviceMethods);
  LogRingBufferFormat(ring_buffer_format);
  if (!ring_buffer_format.pcm_format()) {
    FX_LOGS(WARNING) << "ring_buffer_format must set pcm_format";
    return ZX_ERR_INVALID_ARGS;
  }
  auto& pcm_format = ring_buffer_format.pcm_format().value();
  if (pcm_format.number_of_channels() == 0) {
    FX_LOGS(WARNING) << "RingBuffer number_of_channels is too low";
    return ZX_ERR_OUT_OF_RANGE;
  }
  // Is there an upper limit on RingBuffer channels?

  if (pcm_format.bytes_per_sample() == 0) {
    FX_LOGS(WARNING) << "RingBuffer bytes_per_sample is too low";
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (pcm_format.sample_format() == fuchsia_hardware_audio::SampleFormat::kPcmUnsigned &&
      pcm_format.bytes_per_sample() > sizeof(uint8_t)) {
    FX_LOGS(WARNING) << "RingBuffer bytes_per_sample is too high";
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (pcm_format.sample_format() == fuchsia_hardware_audio::SampleFormat::kPcmSigned &&
      pcm_format.bytes_per_sample() > sizeof(uint32_t)) {
    FX_LOGS(WARNING) << "RingBuffer bytes_per_sample is too high";
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (pcm_format.sample_format() == fuchsia_hardware_audio::SampleFormat::kPcmFloat &&
      pcm_format.bytes_per_sample() > sizeof(double)) {
    FX_LOGS(WARNING) << "RingBuffer bytes_per_sample is too high";
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (pcm_format.valid_bits_per_sample() == 0) {
    FX_LOGS(WARNING) << "RingBuffer valid_bits_per_sample is too low";
    return ZX_ERR_OUT_OF_RANGE;
  }
  auto bytes_per_sample = pcm_format.bytes_per_sample();
  if (pcm_format.valid_bits_per_sample() > bytes_per_sample * 8) {
    FX_LOGS(WARNING) << "RingBuffer valid_bits_per_sample ("
                     << static_cast<uint16_t>(pcm_format.valid_bits_per_sample())
                     << ") cannot exceed bytes_per_sample ("
                     << static_cast<uint16_t>(bytes_per_sample) << ") * 8";
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (pcm_format.frame_rate() > kMaxSupportedRingBufferFrameRate ||
      pcm_format.frame_rate() < kMinSupportedRingBufferFrameRate) {
    FX_LOGS(WARNING) << "RingBuffer frame rate (" << pcm_format.frame_rate()
                     << ") must be within range [" << kMinSupportedRingBufferFrameRate << ", "
                     << kMaxSupportedRingBufferFrameRate << "]";
    return ZX_ERR_OUT_OF_RANGE;
  }

  return ZX_OK;
}

zx_status_t ValidateSampleFormatCompatibility(uint8_t bytes_per_sample,
                                              fuchsia_hardware_audio::SampleFormat sample_format) {
  // Explicitly check for fuchsia_audio::SampleType kUint8, kInt16, kInt32, kFloat32, kFloat64
  if ((sample_format == fuchsia_hardware_audio::SampleFormat::kPcmUnsigned &&
       bytes_per_sample == 1) ||
      (sample_format == fuchsia_hardware_audio::SampleFormat::kPcmSigned &&
       bytes_per_sample == 2) ||
      (sample_format == fuchsia_hardware_audio::SampleFormat::kPcmSigned &&
       bytes_per_sample == 4) ||
      (sample_format == fuchsia_hardware_audio::SampleFormat::kPcmFloat && bytes_per_sample == 4) ||
      (sample_format == fuchsia_hardware_audio::SampleFormat::kPcmFloat && bytes_per_sample == 8)) {
    return ZX_OK;
  }

  FX_LOGS(WARNING) << "No valid fuchsia_audio::SampleType exists, for "
                   << static_cast<uint16_t>(bytes_per_sample) << "-byte " << sample_format;
  return ZX_ERR_INVALID_ARGS;
}

zx_status_t ValidateRingBufferVmo(const zx::vmo& vmo, uint32_t num_frames,
                                  const fuchsia_hardware_audio::Format& rb_format) {
  ADR_LOG(kLogDeviceMethods);
  LogRingBufferVmo(vmo, num_frames, rb_format);

  uint64_t size;
  auto status = ValidateRingBufferFormat(rb_format);
  if (status != ZX_OK) {
    return status;
  }
  status = ValidateSampleFormatCompatibility(rb_format.pcm_format()->bytes_per_sample(),
                                             rb_format.pcm_format()->sample_format());
  if (status != ZX_OK) {
    return status;
  }

  status = vmo.get_size(&size);
  if (status != ZX_OK) {
    FX_LOGS(WARNING) << "get_size returned size " << size << " and error " << status;
    return status;
  }
  if (size < static_cast<uint64_t>(num_frames) * rb_format.pcm_format()->number_of_channels() *
                 rb_format.pcm_format()->bytes_per_sample()) {
    FX_LOGS(WARNING) << "Reported RingBuffer.GetVmo num_frames does not match VMO size";
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t ValidateDelayInfo(const fuchsia_hardware_audio::DelayInfo& delay_info) {
  ADR_LOG(kLogDeviceMethods);
  LogDelayInfo(delay_info);

  if (!delay_info.internal_delay()) {
    FX_LOGS(WARNING) << "Reported DelayInfo.internal_delay is missing";
    return ZX_ERR_INVALID_ARGS;
  }
  const auto internal_delay = *delay_info.internal_delay();
  if (internal_delay < 0) {
    FX_LOGS(WARNING) << "WatchDelayInfo: reported 'internal_delay' (" << internal_delay
                     << " ns) cannot be negative";
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (delay_info.external_delay() && *delay_info.external_delay() < 0) {
    FX_LOGS(WARNING) << "WatchDelayInfo: reported 'external_delay' ("
                     << *delay_info.external_delay() << " ns) cannot be negative";
    return ZX_ERR_OUT_OF_RANGE;
  }

  return ZX_OK;
}

zx_status_t ValidateElementState(
    const fuchsia_hardware_audio_signalprocessing::ElementState& element_state,
    const fuchsia_hardware_audio_signalprocessing::Element& element) {
  LogElementState(element_state);

  if (auto status = ValidateElement(element); status != ZX_OK) {
    return status;
  }

  bool type_specific_matches_element_type;
  switch (*element.type()) {
    case fuchsia_hardware_audio_signalprocessing::ElementType::kVendorSpecific:
      type_specific_matches_element_type = (element_state.type_specific().has_value() &&
                                            element_state.type_specific()->Which() ==
                                                fuchsia_hardware_audio_signalprocessing::
                                                    TypeSpecificElementState::Tag::kVendorSpecific);

      // Need additional vendor-specific checks?
      // Is .vendor_specific_data required?
      break;
    case fuchsia_hardware_audio_signalprocessing::ElementType::kGain:
      type_specific_matches_element_type =
          (element_state.type_specific().has_value() &&
           element_state.type_specific()->Which() ==
               fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::Tag::kGain &&
           element_state.type_specific()->gain().has_value() &&
           element_state.type_specific()->gain()->gain().has_value() &&
           *element_state.type_specific()->gain()->gain() >=
               element.type_specific()->gain()->min_gain() &&
           *element_state.type_specific()->gain()->gain() <=
               element.type_specific()->gain()->max_gain());
      break;
    case fuchsia_hardware_audio_signalprocessing::ElementType::kEqualizer:
      type_specific_matches_element_type =
          (element_state.type_specific().has_value() &&
           element_state.type_specific()->Which() ==
               fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::Tag::kEqualizer &&
           element_state.type_specific()->equalizer().has_value() &&
           element_state.type_specific()->equalizer()->band_states().has_value() &&
           !element_state.type_specific()->equalizer()->band_states()->empty());

      // Need additional EQ-specific checks on each EqualizerBandState.
      break;
    case fuchsia_hardware_audio_signalprocessing::ElementType::kDynamics:
      type_specific_matches_element_type =
          (element_state.type_specific().has_value() &&
           element_state.type_specific()->Which() ==
               fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::Tag::kDynamics &&
           element_state.type_specific()->dynamics().has_value() &&
           element_state.type_specific()->dynamics()->band_states().has_value() &&
           !element_state.type_specific()->dynamics()->band_states()->empty());

      // Need additional dynamics-specific checks on each DynamicsBandState.
      break;
    case fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint:
      type_specific_matches_element_type =
          (element_state.type_specific().has_value() &&
           element_state.type_specific()->Which() ==
               fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::Tag::kEndpoint &&
           element_state.type_specific()->endpoint().has_value() &&
           element_state.type_specific()->endpoint()->plug_state().has_value() &&
           element_state.type_specific()->endpoint()->plug_state()->plugged().has_value() &&
           element_state.type_specific()->endpoint()->plug_state()->plug_state_time().has_value() &&
           (element_state.type_specific()->endpoint()->plug_state()->plugged() ||
            *element.type_specific()->endpoint()->plug_detect_capabilities() ==
                fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kCanAsyncNotify));
      break;
    default:
      type_specific_matches_element_type = true;
      break;
  }

  if (!type_specific_matches_element_type) {
    FX_LOGS(WARNING)
        << "WatchElementState: type_specific is missing or does not match element_type "
        << element.type();
    return ZX_ERR_INVALID_ARGS;
  }

  if (element_state.enabled().has_value()) {
    if (!element.can_disable().value_or(false) && !element_state.enabled().value_or(true)) {
      FX_LOGS(WARNING)
          << "WatchElementState: element_state is disabled, but element.can_disable is false";
      return ZX_ERR_INVALID_ARGS;
    }
  }
  if (element_state.latency().has_value()) {
    if (element_state.latency()->Which() ==
            fuchsia_hardware_audio_signalprocessing::Latency::Tag::kLatencyTime &&
        element_state.latency()->latency_time().value() < 0) {
      FX_LOGS(WARNING) << "WatchElementState: latency, if a duration, must not be negative";
    }
  }

  if (element_state.vendor_specific_data().has_value()) {
    if (element_state.vendor_specific_data()->empty()) {
      FX_LOGS(WARNING) << "WatchElementState: vendor_specific_data, if present, must not be empty";
      return ZX_ERR_INVALID_ARGS;
    }
  }

  return ZX_OK;
}

zx_status_t ValidateElement(const fuchsia_hardware_audio_signalprocessing::Element& element) {
  LogElement(element);

  if (!element.id().has_value()) {
    FX_LOGS(WARNING) << "SignalProcessing.element.id is missing";
    return ZX_ERR_INVALID_ARGS;
  }
  if (!element.type().has_value()) {
    FX_LOGS(WARNING) << "SignalProcessing.element.type is missing";
    return ZX_ERR_INVALID_ARGS;
  }
  if (*element.type() == fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint &&
      !element.type_specific().has_value()) {
    FX_LOGS(WARNING) << "SignalProcessing.element.type is ENDPOINT but type_specific is missing";
    return ZX_ERR_INVALID_ARGS;
  }
  bool mismatch = false;
  if (element.type_specific().has_value()) {
    const auto type = *element.type();
    switch (element.type_specific()->Which()) {
      case fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::Tag::kVendorSpecific:
        mismatch = (type != fuchsia_hardware_audio_signalprocessing::ElementType::kVendorSpecific);
        break;
      case fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::Tag::kGain:
        mismatch = (type != fuchsia_hardware_audio_signalprocessing::ElementType::kGain);
        break;
      case fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::Tag::kEqualizer:
        mismatch = (type != fuchsia_hardware_audio_signalprocessing::ElementType::kEqualizer);
        break;
      case fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::Tag::kDynamics:
        mismatch = (type != fuchsia_hardware_audio_signalprocessing::ElementType::kDynamics);
        break;
      case fuchsia_hardware_audio_signalprocessing::TypeSpecificElement::Tag::kEndpoint:
        mismatch = (type != fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint);
        break;
      default:
        FX_LOGS(WARNING) << "Unknown element.type_specific union found";
        return ZX_ERR_INVALID_ARGS;
    }
    if (mismatch) {
      FX_LOGS(WARNING) << "element(" << *element.id() << "): type " << type
                       << " does not match type_specific union";
      return ZX_ERR_INVALID_ARGS;
    }
  }
  if (element.description().has_value() && element.description()->empty()) {
    FX_LOGS(WARNING) << "SignalProcessing.element.description cannot be empty, if present";
    return ZX_ERR_INVALID_ARGS;
  }

  return ZX_OK;
}

zx_status_t ValidateElements(
    const std::vector<fuchsia_hardware_audio_signalprocessing::Element>& elements) {
  LogElements(elements);

  if (elements.empty()) {
    FX_LOGS(WARNING) << "Reported SignalProcessing.elements[] is empty";
    return ZX_ERR_INVALID_ARGS;
  }

  for (const auto& element : elements) {
    if (auto status = ValidateElement(element); status != ZX_OK) {
      return status;
    }
  }

  return (MapElements(elements).empty() ? ZX_ERR_INVALID_ARGS : ZX_OK);
}

zx_status_t ValidateTopology(const fuchsia_hardware_audio_signalprocessing::Topology& topology,
                             const std::unordered_map<ElementId, ElementRecord>& element_map) {
  LogTopology(topology);
  if (!topology.id().has_value()) {
    FX_LOGS(WARNING) << "Reported SignalProcessing.topology.id is missing";
    return ZX_ERR_INVALID_ARGS;
  }
  if (!topology.processing_elements_edge_pairs().has_value()) {
    FX_LOGS(WARNING)
        << "Reported SignalProcessing.topology.processing_elements_edge_pairs is missing";
    return ZX_ERR_INVALID_ARGS;
  }
  if (topology.processing_elements_edge_pairs()->empty()) {
    FX_LOGS(WARNING)
        << "Reported SignalProcessing.topology.processing_elements_edge_pairs[] is empty";
    return ZX_ERR_INVALID_ARGS;
  }

  std::unordered_set<ElementId> source_elements, destination_elements;
  for (const auto& edge_pair : *topology.processing_elements_edge_pairs()) {
    // Check that all the mentioned element_ids are contained in the elements set.
    if (element_map.find(edge_pair.processing_element_id_from()) == element_map.end()) {
      FX_LOGS(WARNING) << "Element_id_from " << edge_pair.processing_element_id_from()
                       << " not found in element list";
      return ZX_ERR_INVALID_ARGS;
    }
    if (element_map.find(edge_pair.processing_element_id_to()) == element_map.end()) {
      FX_LOGS(WARNING) << "Element_id_to " << edge_pair.processing_element_id_to()
                       << " not found in element list";
      return ZX_ERR_INVALID_ARGS;
    }
    // Check that no EdgePair is self-referential.
    if (edge_pair.processing_element_id_from() == edge_pair.processing_element_id_to()) {
      FX_LOGS(WARNING) << "Edge_pair connects element_id " << edge_pair.processing_element_id_to()
                       << " to itself";
      return ZX_ERR_INVALID_ARGS;
    }

    source_elements.insert(edge_pair.processing_element_id_from());
    destination_elements.insert(edge_pair.processing_element_id_to());
  }

  // Check that only ENDPOINTs are terminal
  for (auto& [id, element_record] : element_map) {
    if (source_elements.find(id) != source_elements.end() &&
        destination_elements.find(id) == destination_elements.end()) {
      if (*element_record.element.type() !=
          fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint) {
        FX_LOGS(WARNING) << "Element " << id << " has no incoming edges but is not an Endpoint! Is "
                         << *element_record.element.type();
        return ZX_ERR_INVALID_ARGS;
      }
    }
    if (source_elements.find(id) == source_elements.end() &&
        destination_elements.find(id) != destination_elements.end()) {
      if (*element_record.element.type() !=
          fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint) {
        FX_LOGS(WARNING) << "Element " << id << " has no outgoing edges but is not an Endpoint! Is "
                         << *element_record.element.type();
        return ZX_ERR_INVALID_ARGS;
      }
    }
  }

  return ZX_OK;
}

zx_status_t ValidateTopologies(
    const std::vector<fuchsia_hardware_audio_signalprocessing::Topology>& topologies,
    const std::unordered_map<ElementId, ElementRecord>& element_map) {
  LogTopologies(topologies);

  if (topologies.empty()) {
    FX_LOGS(WARNING) << "Reported SignalProcessing.topologies[] is empty";
    return ZX_ERR_INVALID_ARGS;
  }
  if (element_map.empty()) {
    FX_LOGS(WARNING) << "Reported SignalProcessing.elements[] is empty";
    return ZX_ERR_INVALID_ARGS;
  }

  // Check each topology
  if (MapTopologies(topologies).empty()) {
    return ZX_ERR_INVALID_ARGS;
  }

  std::unordered_set<ElementId> elements_remaining;
  for (const auto& element_entry_pair : element_map) {
    elements_remaining.insert(element_entry_pair.first);
  }

  for (auto& topology : topologies) {
    if (const auto& status = ValidateTopology(topology, element_map); status != ZX_OK) {
      return status;
    }
    for (const auto& edge_pair : *topology.processing_elements_edge_pairs()) {
      elements_remaining.erase(edge_pair.processing_element_id_from());
      elements_remaining.erase(edge_pair.processing_element_id_to());
    }
  }
  if (!elements_remaining.empty()) {
    FX_LOGS(WARNING) << "topologies did not cover all elements. Example: element_id "
                     << *elements_remaining.begin();
    return ZX_ERR_INVALID_ARGS;
  }

  return ZX_OK;
}

}  // namespace media_audio
