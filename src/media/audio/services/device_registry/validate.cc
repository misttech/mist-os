// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <zircon/errors.h>

#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/logging.h"

namespace media_audio {

// Frame rates must be listed in ascending order, but some drivers don't do this.
// TODO(fxbug.dev/116959): once this is fixed, clean out this workaround.
inline constexpr bool kStrictFrameRateOrdering = false;

// We define these here only temporarily, as we publish no frame-rate limits for audio devices.
// TODO(fxbug.dev/116961): officially define frame-rate limits/expectations for audio devices.
const uint32_t kMinFrameRate = 1000;
const uint32_t kMaxFrameRate = 192000;

namespace {

/////////////////////////////////////////////////////
// Utility functions
// In the enclosed vector<SampleFormat>, how many are 'format_to_match'?
size_t CountFormatMatches(const std::vector<fuchsia_hardware_audio::SampleFormat>& formats,
                          fuchsia_hardware_audio::SampleFormat format_to_match) {
  return std::count_if(formats.begin(), formats.end(),
                       [format_to_match](const auto& format) { return format == format_to_match; });
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

// Translate from fuchsia_hardware_audio::SupportedFormats to fuchsia_audio_device::PcmFormatSet.
std::vector<fuchsia_audio_device::PcmFormatSet> TranslateFormatSets(
    std::vector<fuchsia_hardware_audio::SupportedFormats>& formats) {
  ADR_LOG(kLogDeviceMethods);

  // supported_formats is more complex to copy, since fuchsia_audio_device defines its tables from
  // scratch instead of reusing types from fuchsia_hardware_audio.
  // We build from the inside-out: populating attributes then channel_sets then supported_formats.
  std::vector<fuchsia_audio_device::PcmFormatSet> supported_formats;
  for (auto& fmt : formats) {
    auto& pcm_formats = *fmt.pcm_supported_formats();

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

    // Construct frame_rates on-the-fly.
    std::sort(pcm_formats.frame_rates()->begin(), pcm_formats.frame_rates()->end());
    fuchsia_audio_device::PcmFormatSet pcm_format_set = {{
        .channel_sets = channel_sets,
        .sample_types = sample_types,
        .frame_rates = *pcm_formats.frame_rates(),
    }};
    supported_formats.emplace_back(pcm_format_set);
  }
  return supported_formats;
}

zx_status_t ValidateStreamProperties(
    const fuchsia_hardware_audio::StreamProperties& props,
    std::optional<const fuchsia_hardware_audio::GainState> gain_state,
    std::optional<const fuchsia_hardware_audio::PlugState> plug_state) {
  LogStreamProperties(props);
  ADR_LOG(kLogDeviceMethods);

  if (!props.is_input() || !props.min_gain_db() || !props.max_gain_db() || !props.gain_step_db() ||
      !props.plug_detect_capabilities() || !props.clock_domain()) {
    FX_LOGS(WARNING) << "Incomplete StreamConfig/GetProperties response";
    return ZX_ERR_INVALID_ARGS;
  }

  if (*props.min_gain_db() > *props.max_gain_db()) {
    FX_LOGS(WARNING) << "GetProperties: min_gain_db cannot exceed max_gain_db: "
                     << *props.min_gain_db() << "," << *props.max_gain_db();
    return ZX_ERR_INVALID_ARGS;
  }
  if (*props.gain_step_db() > *props.max_gain_db() - *props.min_gain_db()) {
    FX_LOGS(WARNING) << "GetProperties: gain_step_db cannot exceed max_gain_db-min_gain_db: "
                     << *props.gain_step_db() << "," << *props.max_gain_db() - *props.min_gain_db();
    return ZX_ERR_INVALID_ARGS;
  }
  if (*props.gain_step_db() < 0.0f) {
    FX_LOGS(WARNING) << "GetProperties: gain_step_db (" << *props.gain_step_db()
                     << ") cannot be negative";
    return ZX_ERR_INVALID_ARGS;
  }

  // If we already have this device's GainState, double-check against that.
  if (gain_state) {
    if (*gain_state->gain_db() < *props.min_gain_db() ||
        *gain_state->gain_db() > *props.max_gain_db()) {
      FX_LOGS(WARNING) << "Gain range reported by GetProperties does not include current gain_db: "
                       << *gain_state->gain_db();
      return ZX_ERR_INVALID_ARGS;
    }

    // Device can't mute (or doesn't say it can), but says it is currently muted...
    if (!props.can_mute().value_or(false) && gain_state->muted().value_or(false)) {
      FX_LOGS(WARNING) << "GetProperties reports can_mute FALSE, but device is muted";
      return ZX_ERR_INVALID_ARGS;
    }
    // Device doesn't have AGC (or doesn't say it does), but says AGC is currently enabled...
    if (!props.can_agc().value_or(false) && gain_state->agc_enabled().value_or(false)) {
      FX_LOGS(WARNING) << "GetProperties reports can_agc FALSE, but AGC is enabled";
      return ZX_ERR_INVALID_ARGS;
    }
  }

  // If we already have this device's PlugState, double-check against that.
  if (plug_state && !(*plug_state->plugged()) &&
      *props.plug_detect_capabilities() ==
          fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired) {
    FX_LOGS(WARNING) << "GetProperties reports HARDWIRED, but device is UNPLUGGED";
    return ZX_ERR_INVALID_ARGS;
  }

  return ZX_OK;
}

zx_status_t ValidateSupportedFormats(
    const std::vector<fuchsia_hardware_audio::SupportedFormats>& formats) {
  LogSupportedFormats(formats);
  ADR_LOG(kLogDeviceMethods);

  if (formats.empty()) {
    FX_LOGS(WARNING) << "GetSupportedFormats: supported_formats[] is empty";
    return ZX_ERR_INVALID_ARGS;
  }

  for (const auto& supported_formats : formats) {
    if (!supported_formats.pcm_supported_formats()) {
      FX_LOGS(WARNING) << "GetSupportedFormats: pcm_supported_formats is absent";
      return ZX_ERR_INVALID_ARGS;
    }
    const auto& pcm_supported_formats = *supported_formats.pcm_supported_formats();

    // Frame rates
    [[maybe_unused]] uint32_t prev_frame_rate = 0;
    uint32_t max_supported_frame_rate = 0;
    if (!pcm_supported_formats.frame_rates() || pcm_supported_formats.frame_rates()->empty()) {
      FX_LOGS(WARNING) << "GetSupportedFormats: frame_rates[] is absent/empty";
      return ZX_ERR_INVALID_ARGS;
    }
    for (const auto& rate : *pcm_supported_formats.frame_rates()) {
      if (rate < kMinFrameRate || rate > kMaxFrameRate) {
        FX_LOGS(WARNING) << "GetSupportedFormats: frame_rate (" << rate << ") out of range ["
                         << kMinFrameRate << "," << kMaxFrameRate << "] ";
        return ZX_ERR_OUT_OF_RANGE;
      }
      if constexpr (kStrictFrameRateOrdering) {
        // This also eliminates duplicate entries.
        if (rate <= prev_frame_rate) {
          FX_LOGS(WARNING) << "GetSupportedFormats: frame_rate must be listed in ascending order: "
                           << rate << " listed after " << prev_frame_rate;
          return ZX_ERR_INVALID_ARGS;
        }
        prev_frame_rate = rate;
      } else {
        if (std::count(pcm_supported_formats.frame_rates()->begin(),
                       pcm_supported_formats.frame_rates()->end(), rate) > 1) {
          FX_LOGS(WARNING) << "GetSupportedFormats: rate (" << rate
                           << ") must be unique across frame_rates";
          return ZX_ERR_INVALID_ARGS;
        }
      }

      max_supported_frame_rate = std::max(max_supported_frame_rate, rate);
    }

    // Channel sets
    if (!pcm_supported_formats.channel_sets() || pcm_supported_formats.channel_sets()->empty()) {
      FX_LOGS(WARNING) << "GetSupportedFormats: channel_sets[] is absent/empty";
      return ZX_ERR_INVALID_ARGS;
    }
    for (const fuchsia_hardware_audio::ChannelSet& chan_set :
         *pcm_supported_formats.channel_sets()) {
      if (!chan_set.attributes() || chan_set.attributes()->empty()) {
        FX_LOGS(WARNING) << "GetSupportedFormats: ChannelSet.attributes[] is absent/empty";
        return ZX_ERR_INVALID_ARGS;
      }
      if (CountChannelMatches(*pcm_supported_formats.channel_sets(),
                              chan_set.attributes()->size()) > 1) {
        FX_LOGS(WARNING)
            << "GetSupportedFormats: channel-count must be unique across channel_sets: "
            << chan_set.attributes()->size();
        return ZX_ERR_INVALID_ARGS;
      }
      for (const auto& attrib : *chan_set.attributes()) {
        auto max_allowed_frequency = max_supported_frame_rate / 2;
        if (attrib.min_frequency()) {
          if (*attrib.min_frequency() > max_allowed_frequency) {
            FX_LOGS(WARNING)
                << "GetSupportedFormats: ChannelAttributes.min_frequency out of range: "
                << *attrib.min_frequency();
            return ZX_ERR_OUT_OF_RANGE;
          }
          if (attrib.max_frequency() && *attrib.min_frequency() > *attrib.max_frequency()) {
            FX_LOGS(WARNING) << "GetSupportedFormats: min_frequency cannot exceed max_frequency: "
                             << *attrib.min_frequency() << "," << *attrib.max_frequency();
            return ZX_ERR_INVALID_ARGS;
          }
        }

        if (attrib.max_frequency()) {
          if (*attrib.max_frequency() > max_allowed_frequency) {
            FX_LOGS(WARNING) << "GetSupportedFormats: ChannelAttrib.max_freq "
                             << *attrib.max_frequency() << " will be limited to "
                             << max_allowed_frequency;
          }
        }
      }
    }

    // Sample format
    if (!pcm_supported_formats.sample_formats() ||
        pcm_supported_formats.sample_formats()->empty()) {
      FX_LOGS(WARNING) << "GetSupportedFormats: sample_formats[] is empty";
      return ZX_ERR_INVALID_ARGS;
    }
    const auto& sample_formats = *pcm_supported_formats.sample_formats();
    for (const auto& format : sample_formats) {
      if (CountFormatMatches(sample_formats, format) > 1) {
        FX_LOGS(WARNING) << "GetSupportedFormats: no duplicate SampleFormat values allowed: "
                         << format;
        return ZX_ERR_INVALID_ARGS;
      }
    }

    // Bytes per sample
    if (!pcm_supported_formats.bytes_per_sample() ||
        pcm_supported_formats.bytes_per_sample()->empty()) {
      FX_LOGS(WARNING) << "GetSupportedFormats: bytes_per_sample[] is absent/empty";
      return ZX_ERR_INVALID_ARGS;
    }
    uint8_t prev_bytes_per_sample = 0;
    uint8_t max_bytes_per_sample = 0;
    for (const auto& bytes : *pcm_supported_formats.bytes_per_sample()) {
      if (CountFormatMatches(sample_formats, fuchsia_hardware_audio::SampleFormat::kPcmSigned) &&
          (bytes != 2 && bytes != 4)) {
        FX_LOGS(WARNING)
            << "GetSupportedFormats: bytes_per_sample must be 2 or 4 for PCM_SIGNED format: "
            << bytes;
        return ZX_ERR_INVALID_ARGS;
      }
      if (CountFormatMatches(sample_formats, fuchsia_hardware_audio::SampleFormat::kPcmFloat) &&
          (bytes != 4 && bytes != 8)) {
        FX_LOGS(WARNING)
            << "GetSupportedFormats: bytes_per_sample must be 4 or 8 for PCM_FLOAT format: "
            << bytes;
        return ZX_ERR_INVALID_ARGS;
      }
      if (CountFormatMatches(sample_formats, fuchsia_hardware_audio::SampleFormat::kPcmUnsigned) &&
          bytes != 1) {
        FX_LOGS(WARNING)
            << "GetSupportedFormats: bytes_per_sample must be 1 for PCM_UNSIGNED format: " << bytes;
        return ZX_ERR_INVALID_ARGS;
      }
      // Bytes per sample must be listed in ascending order. This also eliminates duplicate values.
      if (bytes <= prev_bytes_per_sample) {
        FX_LOGS(WARNING)
            << "GetSupportedFormats: bytes_per_sample must be listed in ascending order: " << bytes
            << " listed after " << prev_bytes_per_sample;
        return ZX_ERR_INVALID_ARGS;
      }
      prev_bytes_per_sample = bytes;

      max_bytes_per_sample = std::max(max_bytes_per_sample, bytes);
    }

    // Valid bits per sample
    if (!pcm_supported_formats.valid_bits_per_sample() ||
        pcm_supported_formats.valid_bits_per_sample()->empty()) {
      FX_LOGS(WARNING) << "GetSupportedFormats: valid_bits_per_sample[] is absent/empty";
      return ZX_ERR_INVALID_ARGS;
    }
    uint8_t prev_valid_bits = 0;
    for (const auto& valid_bits : *pcm_supported_formats.valid_bits_per_sample()) {
      if (valid_bits == 0 || valid_bits > max_bytes_per_sample * 8) {
        FX_LOGS(WARNING) << "GetSupportedFormats: valid_bits_per_sample out of range: "
                         << valid_bits;
        return ZX_ERR_OUT_OF_RANGE;
      }
      // Valid bits per sample must be listed in ascending order.
      if (valid_bits <= prev_valid_bits) {
        FX_LOGS(WARNING)
            << "GetSupportedFormats: valid_bits_per_sample must be listed in ascending order: "
            << valid_bits << " listed after " << prev_valid_bits;
        return ZX_ERR_INVALID_ARGS;
      }
      prev_valid_bits = valid_bits;
    }
  }

  return ZX_OK;
}

zx_status_t ValidateGainState(
    const fuchsia_hardware_audio::GainState& gain_state,
    std::optional<const fuchsia_hardware_audio::StreamProperties> stream_props) {
  LogGainState(gain_state);
  ADR_LOG(kLogDeviceMethods);

  if (!gain_state.gain_db()) {
    FX_LOGS(WARNING) << "Incomplete StreamConfig/WatchGainState response";
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
    std::optional<const fuchsia_hardware_audio::StreamProperties> stream_props) {
  LogPlugState(plug_state);
  ADR_LOG(kLogDeviceMethods);

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
  if (stream_props) {
    if (*stream_props->plug_detect_capabilities() ==
            fuchsia_hardware_audio::PlugDetectCapabilities::kHardwired &&
        !plug_state.plugged().value_or(true)) {
      FX_LOGS(WARNING) << "Reported 'plug_state' (UNPLUGGED) is unsupported (HARDWIRED)";
      return ZX_ERR_INVALID_ARGS;
    }
  }

  return ZX_OK;
}

// Validate only DeviceInfo-specific aspects. For example, don't re-validate format correctness.
bool ValidateDeviceInfo(const fuchsia_audio_device::Info& device_info) {
  LogDeviceInfo(device_info);
  ADR_LOG(kLogDeviceMethods);

  // Validate top-level required members.
  if (!device_info.token_id() || !device_info.device_type() || !device_info.device_name() ||
      device_info.device_name()->empty() || !device_info.supported_formats() ||
      device_info.supported_formats()->empty() || !device_info.gain_caps() ||
      !device_info.plug_detect_caps() || !device_info.clock_domain()) {
    FX_LOGS(WARNING) << __func__ << ": incomplete DeviceInfo instance";
    return false;
  }

  return true;
}

zx_status_t ValidateRingBufferProperties(
    const fuchsia_hardware_audio::RingBufferProperties& rb_props) {
  LogRingBufferProperties(rb_props);
  ADR_LOG(kLogDeviceMethods);

  if (rb_props.external_delay() && *rb_props.external_delay() < 0) {
    FX_LOGS(WARNING) << "Reported RingBufferProperties.external_delay is negative";
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (rb_props.turn_on_delay() && *rb_props.turn_on_delay() < 0) {
    FX_LOGS(WARNING) << "Reported RingBufferProperties.external_delay is negative";
    return ZX_ERR_OUT_OF_RANGE;
  }
  return ZX_OK;
}

zx_status_t ValidateRingBufferFormat(const fuchsia_hardware_audio::Format& format) {
  LogRingBufferFormat(format);
  ADR_LOG(kLogDeviceMethods);
  if (!format.pcm_format()) {
    FX_LOGS(WARNING) << "RingBuffer format must set pcm_format";
    return ZX_ERR_INVALID_ARGS;
  }

  if (format.pcm_format()->valid_bits_per_sample() > format.pcm_format()->bytes_per_sample() * 8) {
    FX_LOGS(WARNING) << "RingBuffer valid_bits_per_sample ("
                     << format.pcm_format()->valid_bits_per_sample()
                     << ") cannot exceed bytes_per_sample ("
                     << format.pcm_format()->bytes_per_sample() << ") * 8";
    return ZX_ERR_OUT_OF_RANGE;
  }
  if (format.pcm_format()->frame_rate() > kMaxFrameRate ||
      format.pcm_format()->frame_rate() < kMinFrameRate) {
    FX_LOGS(WARNING) << "RingBuffer frame rate (" << format.pcm_format()->frame_rate()
                     << ") must be within range [" << kMinFrameRate << ", " << kMaxFrameRate << "]";
    return ZX_ERR_OUT_OF_RANGE;
  }

  return ZX_OK;
}

zx_status_t ValidateRingBufferVmo(const zx::vmo& vmo, uint32_t num_frames,
                                  const fuchsia_hardware_audio::Format& format) {
  LogRingBufferVmo(vmo, num_frames, format);
  ADR_LOG(kLogDeviceMethods);

  uint64_t size;
  auto status = ValidateRingBufferFormat(format);
  if (status != ZX_OK) {
    return status;
  }

  status = vmo.get_size(&size);
  if (status != ZX_OK) {
    FX_LOGS(WARNING) << "get_size returned size " << size << " and error " << status;
    return status;
  }
  if (size < num_frames * format.pcm_format()->number_of_channels() *
                 format.pcm_format()->bytes_per_sample()) {
    FX_LOGS(WARNING) << "Reported RingBuffer.GetVmo num_frames does not match VMO size";
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t ValidateDelayInfo(
    const fuchsia_hardware_audio::DelayInfo& delay_info,
    const std::optional<const fuchsia_hardware_audio::RingBufferProperties>& rb_props,
    const fuchsia_hardware_audio::PcmFormat& format) {
  LogDelayInfo(delay_info);
  ADR_LOG(kLogDeviceMethods);

  const auto int_delay = delay_info.internal_delay().value_or(0);
  const auto ext_delay = delay_info.external_delay().value_or(0);
  if (int_delay < 0) {
    FX_LOGS(WARNING) << "WatchDelayInfo: reported 'internal_delay' (" << int_delay
                     << " ns) cannot be negative";
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (ext_delay < 0) {
    FX_LOGS(WARNING) << "WatchDelayInfo: reported 'external_delay' (" << ext_delay
                     << " ns) cannot be negative";
    return ZX_ERR_OUT_OF_RANGE;
  }

  // If we have (redundant) delay values from RingBufferProperties, double-check against those.
  if (rb_props) {
    if (rb_props->external_delay() && *rb_props->external_delay() != ext_delay) {
      FX_LOGS(WARNING) << "RingBufferProperties reported 'external_delay' ("
                       << *rb_props->external_delay()
                       << " ns) must match WatchDelayInfo 'external_delay' (" << ext_delay
                       << " ns)";
      return ZX_ERR_INVALID_ARGS;
    }
  }

  return ZX_OK;
}

}  // namespace media_audio
