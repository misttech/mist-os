// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/audio_core/v1/audio_driver.h"

#include <fuchsia/media/cpp/fidl.h>
#include <lib/async/cpp/time.h>
#include <lib/fidl/cpp/clone.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/trace/event.h>
#include <lib/zx/clock.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include <algorithm>
#include <cstdio>
#include <iomanip>
#include <sstream>

#include <audio-proto-utils/format-utils.h>
#include <fbl/algorithm.h>

#include "src/lib/fxl/strings/string_printf.h"
#include "src/media/audio/audio_core/shared/select_best_format.h"
#include "src/media/audio/audio_core/v1/logging_flags.h"
#include "src/media/audio/lib/clock/audio_clock_coefficients.h"
#include "src/media/audio/lib/clock/clone_mono.h"
#include "src/media/audio/lib/clock/utils.h"
#include "src/media/audio/lib/format/driver_format.h"

namespace media::audio {
namespace {

// TODO(fxbug.dev/39092): Log a cobalt metric for this.
void LogMissedCommandDeadline(zx::duration delay, const std::string& cmd_tag) {
  FX_LOGS(WARNING) << "Driver command '" << cmd_tag << "' missed deadline by " << delay.to_nsecs()
                   << "ns";
}

}  // namespace

AudioDriver::AudioDriver(AudioDevice* owner) : AudioDriver(owner, LogMissedCommandDeadline) {}

AudioDriver::AudioDriver(AudioDevice* owner, DriverTimeoutHandler timeout_handler)
    : owner_(owner),
      timeout_handler_(std::move(timeout_handler)),
      versioned_ref_time_to_frac_presentation_frame_(
          fbl::MakeRefCounted<VersionedTimelineFunction>()) {
  FX_DCHECK(owner_ != nullptr);
}

zx_status_t AudioDriver::Init(zx::channel stream_channel) {
  TRACE_DURATION("audio", "AudioDriver::Init");
  // TODO(fxbug.dev/13665): Figure out a better way to assert this!
  OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());
  FX_DCHECK(state_ == State::Uninitialized);

  // Fetch the KOID of our stream channel. We use this unique ID as our device's device token.
  zx_status_t res;
  zx_info_handle_basic_t sc_info;
  res = stream_channel.get_info(ZX_INFO_HANDLE_BASIC, &sc_info, sizeof(sc_info), nullptr, nullptr);
  if (res != ZX_OK) {
    FX_PLOGS(ERROR, res) << "Failed to to fetch stream channel KOID";
    return res;
  }
  stream_channel_koid_ = sc_info.koid;

  stream_config_fidl_ =
      fidl::InterfaceHandle<fuchsia::hardware::audio::StreamConfig>(std::move(stream_channel))
          .Bind();
  if (!stream_config_fidl_.is_bound()) {
    FX_LOGS(ERROR) << "Failed to get stream channel";
    return ZX_ERR_INTERNAL;
  }
  stream_config_fidl_.set_error_handler([this](zx_status_t status) -> void {
    OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());
    if constexpr (kLogAudioDriverCallbacks) {
      FX_LOGS(INFO) << "stream_config error_handler (driver " << this << ")";
    }
    ShutdownSelf("Stream channel closed", status);
  });

  cmd_timeout_.set_handler([this] {
    OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());
    DriverCommandTimedOut();
  });

  // We are now initialized, but we don't know any fundamental driver level info, such as:
  //
  // 1) This device's persistent unique ID.
  // 2) The list of formats supported by this device.
  // 3) The user-visible strings for this device (manufacturer, product, etc...).
  state_ = State::MissingDriverInfo;
  return ZX_OK;
}

void AudioDriver::Cleanup() {
  TRACE_DURATION("audio", "AudioDriver::Cleanup");
  // TODO(fxbug.dev/13665): Figure out a better way to assert this!
  OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());
  std::shared_ptr<ReadableRingBuffer> readable_ring_buffer;
  std::shared_ptr<WritableRingBuffer> writable_ring_buffer;
  {
    std::lock_guard<std::mutex> lock(ring_buffer_state_lock_);
    readable_ring_buffer = std::move(readable_ring_buffer_);
    writable_ring_buffer = std::move(writable_ring_buffer_);
  }
  versioned_ref_time_to_frac_presentation_frame_->Update({});
  readable_ring_buffer = nullptr;
  writable_ring_buffer = nullptr;

  cmd_timeout_.Cancel();
  stream_config_fidl_ = nullptr;
  ring_buffer_fidl_ = nullptr;
}

std::optional<Format> AudioDriver::GetFormat() const {
  TRACE_DURATION("audio", "AudioDriver::GetFormat");
  std::lock_guard<std::mutex> lock(configured_format_lock_);
  return configured_format_;
}

zx_status_t AudioDriver::GetDriverInfo() {
  TRACE_DURATION("audio", "AudioDriver::GetDriverInfo");
  // TODO(fxbug.dev/13665): Figure out a better way to assert this!
  OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());

  // We have to be operational in order to fetch supported formats.
  if (!operational()) {
    FX_LOGS(ERROR) << "Cannot fetch supported formats while non-operational (state = "
                   << static_cast<uint32_t>(state_) << ")";
    return ZX_ERR_BAD_STATE;
  }

  // If already fetching initial driver info, get out now and inform our owner when this completes.
  if (fetching_driver_info()) {
    return ZX_OK;
  }
  fetched_driver_info_ = kStartedFetchingDriverInfo;

  // Send the commands to get:
  // - persistent unique ID.
  // - manufacturer string.
  // - product string.
  // - gain capabilities.
  // - current gain state.
  // - supported format list.
  // - clock domain.

  // Get unique IDs, strings and gain capabilities.
  stream_config_fidl_->GetProperties([this](fuchsia::hardware::audio::StreamProperties props) {
    OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());
    if (state_ != State::MissingDriverInfo) {
      FX_LOGS(ERROR) << "Bad state (" << static_cast<uint32_t>(state_)
                     << ") while handling get string response.";
      ShutdownSelf("Bad state.", ZX_ERR_INTERNAL);
    }
    hw_gain_state_.can_mute = props.has_can_mute() && props.can_mute();
    hw_gain_state_.can_agc = props.has_can_agc() && props.can_agc();
    hw_gain_state_.min_gain = props.min_gain_db();
    hw_gain_state_.max_gain = props.max_gain_db();
    hw_gain_state_.gain_step = props.gain_step_db();

    if (props.has_unique_id()) {
      std::memcpy(persistent_unique_id_.data, props.unique_id().data(),
                  sizeof(persistent_unique_id_.data));
    }

    if (props.has_manufacturer()) {
      manufacturer_name_ = props.manufacturer();
    }
    if (props.has_product()) {
      product_name_ = props.product();
    }

    clock_domain_ = props.clock_domain();
    FX_LOGS(DEBUG) << "Received clock domain " << clock_domain_;

    // Now that we have our clock domain, we can establish our audio device clock
    SetUpClocks();

    auto res = OnDriverInfoFetched(kDriverInfoHasUniqueId | kDriverInfoHasMfrStr |
                                   kDriverInfoHasProdStr | kDriverInfoHasClockDomain);
    if (res != ZX_OK) {
      ShutdownSelf("Failed to update info fetched.", res);
    }

    pd_hardwired_ = (props.plug_detect_capabilities() ==
                     fuchsia::hardware::audio::PlugDetectCapabilities::HARDWIRED);
  });

  // Get current gain state.
  // We only fetch once per OnDriverInfoFetched, the we are guaranteed by the
  // audio driver interface definition that the driver will reply to the first watch request, we
  // can get the gain state by issuing a watch FIDL call.
  stream_config_fidl_->WatchGainState([this](fuchsia::hardware::audio::GainState state) {
    OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());
    hw_gain_state_.cur_mute = state.has_muted() && state.muted();
    hw_gain_state_.cur_agc = state.has_agc_enabled() && state.agc_enabled();
    hw_gain_state_.cur_gain = state.gain_db();
    auto res = OnDriverInfoFetched(kDriverInfoHasGainState);
    if (res != ZX_OK) {
      ShutdownSelf("Failed to update info fetched.", res);
    }
  });

  // Get list of supported formats.
  stream_config_fidl_->GetSupportedFormats(
      [this](std::vector<fuchsia::hardware::audio::SupportedFormats> formats) {
        OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());
        formats_.reserve(formats.size());
        for (auto& i : formats) {
          formats_.emplace_back(std::move(*i.mutable_pcm_supported_formats()));
        }
        // Record that we fetched the format list. This transitions us to Unconfigured state and
        // tells our owner whether we have fetched all the initial driver info needed to operate.
        auto res = OnDriverInfoFetched(kDriverInfoHasFormats);
        if (res != ZX_OK) {
          ShutdownSelf("Failed to update info fetched.", res);
        }
      });

  // Setup our command timeout.
  SetCommandTimeout(
      kDefaultShortCmdTimeout,
      "Fetch driver info (StreamConfig::GetProperties/GetSupportedFormats/WatchGainState)");
  return ZX_OK;
}

// Confirm that PcmSupportedFormats is well-formed (return false if not) and log the contents.
bool AudioDriver::ValidatePcmSupportedFormats(
    std::vector<fuchsia::hardware::audio::PcmSupportedFormats>& formats, bool is_input) {
  for (size_t format_index = 0u; format_index < formats.size(); ++format_index) {
    if constexpr (kLogAudioDriverFormats || kLogIdlePolicyChannelFrequencies) {
      FX_LOGS(INFO) << "AudioDriver::" << __FUNCTION__ << ": " << (is_input ? " Input" : "Output")
                    << " PcmSupportedFormats[" << format_index << "] for "
                    << (is_input ? " Input" : "Output");
    }

    if (!formats[format_index].has_channel_sets()) {
      FX_LOGS(WARNING) << (is_input ? " Input" : "Output") << " PcmSupportedFormats["
                       << format_index << "] table does not have required ChannelSets";
      return false;
    }

    if (formats[format_index].frame_rates().empty()) {
      FX_LOGS(WARNING) << (is_input ? " Input" : "Output") << " PcmSupportedFormats["
                       << format_index << "].frame_rates contains no entries";
      return false;
    } else {
      if constexpr (kLogAudioDriverFormats) {
        std::ostringstream out;
        for (const auto rate : formats[format_index].frame_rates()) {
          out << rate << " ";
        }
        FX_LOGS(INFO) << " frame_rates: [ " << out.str() << "]";
      }
    }

    auto& channel_sets = formats[format_index].channel_sets();
    for (size_t channel_set_index = 0u; channel_set_index < channel_sets.size();
         ++channel_set_index) {
      auto& channel_set = channel_sets[channel_set_index];
      if (!channel_set.has_attributes()) {
        FX_LOGS(WARNING) << (is_input ? " Input" : "Output") << " PcmSupportedFormats["
                         << format_index << "].channel_sets[" << channel_set_index
                         << "] table does not have required attributes";
        return false;
      }

      if constexpr (kLogAudioDriverFormats || kLogIdlePolicyChannelFrequencies) {
        auto& chan_set_attribs = channel_set.attributes();
        for (size_t channel_index = 0u; channel_index < chan_set_attribs.size(); ++channel_index) {
          std::ostringstream out;
          out << (is_input ? " Input" : "Output") << " PcmSupportedFormats[" << format_index
              << "].channel_sets[" << channel_set_index << "].channel[" << channel_index
              << "] Min: ";
          if (chan_set_attribs[channel_index].has_min_frequency()) {
            out << chan_set_attribs[channel_index].min_frequency();
          } else {
            out << "NONE";
          }
          out << ", Max: ";
          if (chan_set_attribs[channel_index].has_max_frequency()) {
            out << chan_set_attribs[channel_index].max_frequency();
          } else {
            out << "NONE";
          }
          FX_LOGS(INFO) << out.str();
        }
      }
    }
  }

  return true;
}

zx_status_t AudioDriver::Configure(const Format& format, zx::duration min_ring_buffer_duration) {
  TRACE_DURATION("audio", "AudioDriver::Configure");
  // TODO(fxbug.dev/13665): Figure out a better way to assert this!
  OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());

  uint32_t channels = format.channels();
  uint32_t frames_per_second = format.frames_per_second();
  fuchsia::media::AudioSampleFormat sample_format = format.sample_format();

  // Sanity check arguments.
  if (channels > std::numeric_limits<uint16_t>::max()) {
    FX_LOGS(ERROR) << "Bad channel count: " << channels;
    return ZX_ERR_INVALID_ARGS;
  }

  // TODO(fxbug.dev/13666): sanity check the min_ring_buffer_duration.

  // Check our known format list for compatibility.
  if (!IsFormatInSupported(format.stream_type(), formats_)) {
    FX_LOGS(ERROR) << "No compatible format found when setting format to " << frames_per_second
                   << " Hz " << channels << " Ch Fmt 0x" << std::hex
                   << static_cast<uint32_t>(sample_format);
    return ZX_ERR_INVALID_ARGS;
  }

  // We must be in Unconfigured state to change formats.
  // TODO(fxbug.dev/13667): Also permit this if we are in Configured state.
  if (state_ != State::Unconfigured) {
    FX_LOGS(ERROR) << "Bad state while attempting to configure for " << frames_per_second << " Hz "
                   << channels << " Ch Fmt 0x" << std::hex << static_cast<uint32_t>(sample_format)
                   << " (state = " << static_cast<uint32_t>(state_) << ")";
    return ZX_ERR_BAD_STATE;
  }

  bool is_input = owner_->is_input();
  if (!ValidatePcmSupportedFormats(formats_, is_input)) {
    return ZX_ERR_INTERNAL;
  }

  // Retrieve the relevant ChannelSet; stop looking through all formats/sets when we find a match.
  bool found_channel_set_match = false;
  std::vector<ChannelAttributes> channel_config;
  uint32_t max_rate = 0;
  for (auto& format : formats_) {
    max_rate = std::max(*std::max_element(format.frame_rates().begin(), format.frame_rates().end()),
                        max_rate);
  }
  for (auto& format : formats_) {
    for (auto& channel_set : format.channel_sets()) {
      auto& chan_set_attribs = channel_set.attributes();
      if (chan_set_attribs.size() != channels) {
        continue;
      }
      for (size_t channel_index = 0u; channel_index < chan_set_attribs.size(); ++channel_index) {
        // If a frequency range doesn't specify min or max, assume it extends to the boundary.
        channel_config.push_back({chan_set_attribs[channel_index].has_min_frequency()
                                      ? chan_set_attribs[channel_index].min_frequency()
                                      : 0u,
                                  chan_set_attribs[channel_index].has_max_frequency()
                                      ? chan_set_attribs[channel_index].max_frequency()
                                      : (max_rate / 2)});
      }
      found_channel_set_match = true;
      break;
    }
    if (found_channel_set_match) {
      break;
    }
  }

  // Record the details of our intended target format
  min_ring_buffer_duration_ = min_ring_buffer_duration;
  {
    std::lock_guard<std::mutex> lock(configured_format_lock_);
    configured_format_ = {format};
    configured_channel_config_.swap(channel_config);
  }

  if constexpr (kLogIdlePolicyChannelFrequencies) {
    if (channels != configured_channel_config_.size()) {
      FX_LOGS(WARNING) << "Logic error, retrieved a channel_config of incorrect length (wanted "
                       << channels << ", got " << configured_channel_config_.size();
      return ZX_ERR_INTERNAL;
    }
    for (size_t channel_index = 0u; channel_index < channels; ++channel_index) {
      FX_LOGS(INFO) << "Final configured_channel_config_[" << channel_index << "] is ("
                    << configured_channel_config_[channel_index].min_frequency << ", "
                    << configured_channel_config_[channel_index].max_frequency << ") for "
                    << (is_input ? " Input" : "Output");
    }
  }

  zx::channel local_channel;
  zx::channel remote_channel;
  zx_status_t status = zx::channel::create(0u, &local_channel, &remote_channel);
  if (status != ZX_OK) {
    FX_LOGS(ERROR) << "Bad status creating channel: " << status;
    return ZX_ERR_BAD_STATE;
  }
  fidl::InterfaceRequest<fuchsia::hardware::audio::RingBuffer> request = {};
  request.set_channel(std::move(remote_channel));

  DriverSampleFormat driver_format = {};
  if (!AudioSampleFormatToDriverSampleFormat(format.stream_type().sample_format, &driver_format)) {
    FX_LOGS(ERROR) << "Failed to convert Fmt 0x" << std::hex << static_cast<uint32_t>(sample_format)
                   << " to driver format.";
    return ZX_ERR_INVALID_ARGS;
  }

  fuchsia::hardware::audio::Format fidl_format = {};
  fuchsia::hardware::audio::PcmFormat pcm = {};
  pcm.number_of_channels = channels;
  pcm.bytes_per_sample = format.bytes_per_frame() / channels;
  pcm.valid_bits_per_sample = format.valid_bits_per_channel();
  pcm.frame_rate = frames_per_second;
  pcm.sample_format = driver_format.sample_format;
  fidl_format.set_pcm_format(std::move(pcm));

  if (!stream_config_fidl_.is_bound()) {
    FX_LOGS(ERROR) << "Stream channel lost";
    return ZX_ERR_INTERNAL;
  }

  if constexpr (kLogAudioDriverFormats) {
    auto format_str = [](DriverSampleFormat driver_format) {
      switch (driver_format.sample_format) {
        case fuchsia::hardware::audio::SampleFormat::PCM_SIGNED:
          return "signed";
        case fuchsia::hardware::audio::SampleFormat::PCM_UNSIGNED:
          return "unsigned";
        case fuchsia::hardware::audio::SampleFormat::PCM_FLOAT:
          return "float";
        default:
          return "unknown";
      }
    };
    FX_LOGS(INFO) << "AudioDriver: CreateRingBuffer with format [chans: " << channels << ", "
                  << format_str(driver_format) << " " << format.valid_bits_per_channel() << "-in-"
                  << (format.bytes_per_frame() * 8 / channels) << ", " << frames_per_second
                  << " Hz] for " << (is_input ? "INPUT" : "OUTPUT") << " driver " << this;
  }

  FX_CHECK(state_ == State::Unconfigured);
  state_ = State::Configuring_SettingFormat;
  stream_config_fidl_->CreateRingBuffer(std::move(fidl_format), std::move(request));
  // No need for a driver command timeout: there is no reply to this FIDL message.

  ring_buffer_fidl_ =
      fidl::InterfaceHandle<fuchsia::hardware::audio::RingBuffer>(std::move(local_channel)).Bind();
  if (!ring_buffer_fidl_.is_bound()) {
    FX_LOGS(ERROR) << "Failed to get stream channel";
    return ZX_ERR_INTERNAL;
  }
  ring_buffer_fidl_.set_error_handler([this](zx_status_t status) -> void {
    if constexpr (kLogAudioDriverCallbacks) {
      FX_LOGS(INFO) << "ring_buffer error_handler (driver " << this << ")";
    }
    OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());
    ShutdownSelf("Ring buffer channel closed unexpectedly", status);
  });

  RequestRingBufferProperties();
  return ZX_OK;
}

void AudioDriver::RequestRingBufferProperties() {
  // Change state, setup our command timeout.
  FX_CHECK(state_ == State::Configuring_SettingFormat);
  state_ = State::Configuring_GettingRingBufferProperties;
  SetCommandTimeout(kDefaultLongCmdTimeout, "RingBuffer::GetProperties");

  ring_buffer_fidl_->GetProperties([this](fuchsia::hardware::audio::RingBufferProperties props) {
    if constexpr (kLogAudioDriverCallbacks) {
      FX_LOGS(INFO) << "AudioDriver::ring_buffer_fidl::GetProperties callback";
    }

    OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());
    turn_on_delay_ = zx::nsec(props.has_turn_on_delay() ? props.turn_on_delay() : 0);

    // TODO(fxbug.dev/113705): obey the flag when it is false. We behave as if it is always true.
    needs_cache_flush_or_invalidate_ = props.has_needs_cache_flush_or_invalidate()
                                           ? props.needs_cache_flush_or_invalidate()
                                           : true;

    if constexpr (kLogDriverDelayProperties) {
      FX_LOGS(INFO) << "Audio " << (owner_->is_input() ? " Input" : "Output")
                    << " received turn_on_delay  " << std::setw(5) << turn_on_delay_.to_usecs()
                    << " usec";
    }

    RequestDelayInfo();
  });
}

void AudioDriver::RequestDelayInfo() {
  // Change state, setup our command timeout.
  FX_CHECK(state_ == State::Configuring_GettingRingBufferProperties);
  state_ = State::Configuring_GettingDelayInfo;
  SetCommandTimeout(kDefaultLongCmdTimeout, "RingBuffer::WatchDelayInfo");

  ring_buffer_fidl_->WatchDelayInfo([this](fuchsia::hardware::audio::DelayInfo info) {
    if constexpr (kLogAudioDriverCallbacks) {
      FX_LOGS(INFO) << "AudioDriver::ring_buffer_fidl::WatchDelayInfo callback";
    }

    OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());
    external_delay_ = zx::nsec(info.has_external_delay() ? info.external_delay() : 0);
    internal_delay_ = zx::nsec(info.has_internal_delay() ? info.internal_delay() : 0);
    internal_delay_frames_ = internal_delay_.to_nsecs()
                                 // If internal_delay is non-zero, ceiling up to the next frame.
                                 ? (internal_delay_.to_nsecs() * GetFormat()->frames_per_second() -
                                    1) / zx::sec(1).to_nsecs() +
                                       1
                                 : 0;

    if constexpr (kLogDriverDelayProperties) {
      FX_LOGS(INFO) << "Audio " << (owner_->is_input() ? " Input" : "Output")
                    << " received external_delay " << std::setw(5) << external_delay_.to_usecs()
                    << " usec";
      FX_LOGS(INFO) << "Audio " << (owner_->is_input() ? " Input" : "Output")
                    << " received internal_delay " << std::setw(5) << internal_delay_.to_usecs()
                    << " usec (" << internal_delay_frames_ << " frames)";
    }

    auto format = GetFormat();
    auto bytes_per_frame = format->bytes_per_frame();
    auto frames_per_second = format->frames_per_second();

    // Figure out how many frames we need in our ring buffer.
    TimelineRate bytes_per_nanosecond(bytes_per_frame * frames_per_second, ZX_SEC(1));
    int64_t min_bytes_64 = bytes_per_nanosecond.Scale(min_ring_buffer_duration_.to_nsecs());
    bool overflow = ((min_bytes_64 == TimelineRate::kOverflow) ||
                     (min_bytes_64 > (std::numeric_limits<int64_t>::max() -
                                      (internal_delay_frames_ * bytes_per_frame))));

    int64_t min_frames_64;
    if (!overflow) {
      min_frames_64 = min_bytes_64 / bytes_per_frame;
      min_frames_64 += internal_delay_frames_;
      overflow = min_frames_64 > std::numeric_limits<uint32_t>::max();
    }

    if (overflow) {
      FX_LOGS(ERROR) << "Overflow while attempting to compute ring buffer size in frames.";
      FX_LOGS(ERROR) << "duration        : " << min_ring_buffer_duration_.get();
      FX_LOGS(ERROR) << "bytes per frame : " << bytes_per_frame;
      FX_LOGS(ERROR) << "frames per sec  : " << frames_per_second;
      FX_LOGS(ERROR) << "int_delay_frames: " << internal_delay_frames_;
      return;
    }

    RequestRingBufferVmo(min_frames_64);

    // TODO(fxbug.dev/113710): Watch for subsequent delay updates.
  });
}

void AudioDriver::RequestRingBufferVmo(int64_t min_frames_64) {
  // Change state, setup our command timeout.
  FX_CHECK(state_ == State::Configuring_GettingDelayInfo);
  state_ = State::Configuring_GettingRingBufferVmo;
  SetCommandTimeout(kDefaultLongCmdTimeout, "RingBuffer::GetVmo");

  auto num_notifications_per_ring =
      ((clock_domain_ == fuchsia::hardware::audio::CLOCK_DOMAIN_MONOTONIC)) ? 0 : 2;
  ring_buffer_fidl_->GetVmo(
      static_cast<uint32_t>(min_frames_64), num_notifications_per_ring,
      [this](fuchsia::hardware::audio::RingBuffer_GetVmo_Result result) {
        if constexpr (kLogAudioDriverCallbacks) {
          FX_LOGS(INFO) << "AudioDriver::ring_buffer_fidl::GetVmo callback";
        }

        OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());
        {
          std::lock_guard<std::mutex> lock(ring_buffer_state_lock_);
          auto format = GetFormat();
          if (owner_->is_input()) {
            readable_ring_buffer_ = BaseRingBuffer::CreateReadableHardwareBuffer(
                *format, versioned_ref_time_to_frac_presentation_frame_, reference_clock(),
                std::move(result.response().ring_buffer), result.response().num_frames, [this]() {
                  OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());
                  auto t = reference_clock()->now();
                  // Safe-read position: ring-buffer readers should never BEYOND this frame.
                  // We Floor any fractional-frame position to be conservative ("safe").
                  return Fixed::FromRaw(ref_time_to_frac_safe_read_or_write_frame_.Apply(t.get()))
                      .Floor();
                });
          } else {
            writable_ring_buffer_ = BaseRingBuffer::CreateWritableHardwareBuffer(
                *format, versioned_ref_time_to_frac_presentation_frame_, reference_clock(),
                std::move(result.response().ring_buffer), result.response().num_frames, [this]() {
                  OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());
                  auto t = reference_clock()->now();
                  // Safe-write position: ring-buffer writers should always write AT/BEYOND this
                  // frame. We Ceiling any fractional-frame position to be conservative ("safe").
                  return Fixed::FromRaw(ref_time_to_frac_safe_read_or_write_frame_.Apply(t.get()))
                      .Ceiling();
                });
          }
          if (!readable_ring_buffer_ && !writable_ring_buffer_) {
            ShutdownSelf("Failed to allocate and map driver ring buffer", ZX_ERR_NO_MEMORY);
            return;
          }
          FX_DCHECK(!versioned_ref_time_to_frac_presentation_frame_->get().first.invertible());

          ring_buffer_size_bytes_ = format->bytes_per_frame() * result.response().num_frames;
          running_pos_bytes_ = 0;
          frac_frames_per_byte_ = TimelineRate(Fixed(1).raw_value(), format->bytes_per_frame());
        }

        // We are now Configured. Let our owner know about this important milestone.
        state_ = State::Configured;
        ClearCommandTimeout();
        owner_->OnDriverConfigComplete();

        RequestNextPlugStateChange();

        if (clock_domain_ != Clock::kMonotonicDomain) {
          RequestNextClockRecoveryUpdate();
        }
      });
}

void AudioDriver::RequestNextPlugStateChange() {
  stream_config_fidl_->WatchPlugState([this](fuchsia::hardware::audio::PlugState state) {
    if constexpr (kLogAudioDriverCallbacks) {
      FX_LOGS(INFO) << "AudioDriver::WatchPlugState callback";
    }

    OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());
    // Wardware reporting hardwired but notifies unplugged.
    if (pd_hardwired_ && !state.plugged()) {
      FX_LOGS(WARNING) << "Stream reports hardwired yet notifies unplugged, notifying as plugged";
      ReportPlugStateChange(true, zx::time(state.plug_state_time()));
      return;
    }
    ReportPlugStateChange(state.plugged(), zx::time(state.plug_state_time()));
    RequestNextPlugStateChange();
  });
  // No need for a driver command timeout: this is a "hanging get".
}

// This position notification will be used to synthesize a clock for this audio device.
void AudioDriver::ClockRecoveryUpdate(fuchsia::hardware::audio::RingBufferPositionInfo info) {
  TRACE_DURATION("audio", "AudioDriver::ClockRecoveryUpdate");
  if (clock_domain_ == Clock::kMonotonicDomain) {
    return;
  }

  OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());

  FX_CHECK(state_ == State::Started)
      << "ClockRecovery update while in state " << static_cast<uint32_t>(state_) << " -- should be "
      << static_cast<uint32_t>(State::Started);

  auto actual_mono_time = zx::time(info.timestamp);
  FX_CHECK(actual_mono_time >= mono_start_time_) << "Position notification while not started";

  // Based on (wraparound) ring positions, we maintain a long-running byte position
  auto prev_ring_position = running_pos_bytes_ % ring_buffer_size_bytes_;
  running_pos_bytes_ -= prev_ring_position;
  running_pos_bytes_ += info.position;
  // If previous position >= this new position, we must have wrapped around
  // The only exception: the first position notification (comparing to default initialized values)
  if (prev_ring_position >= info.position && actual_mono_time > mono_start_time_) {
    running_pos_bytes_ += ring_buffer_size_bytes_;
  }

  FX_CHECK(recovered_clock_);
  auto predicted_mono_time = recovered_clock_->Update(actual_mono_time, running_pos_bytes_);

  if constexpr (kDriverPositionNotificationDisplayInterval > 0) {
    if (position_notification_count_ % kDriverPositionNotificationDisplayInterval == 0) {
      auto curr_error = predicted_mono_time - actual_mono_time;
      FX_LOGS(INFO) << static_cast<void*>(this) << " " << recovered_clock_->name()
                    << " notification #" << position_notification_count_ << " [" << info.timestamp
                    << ", " << std::setw(6) << info.position << "] run_pos_bytes "
                    << running_pos_bytes_ << ", run_time "
                    << (actual_mono_time - mono_start_time_).get() << ", predicted_mono "
                    << predicted_mono_time.get() << ", curr_err " << curr_error.get();
    }
  }

  // Maintain a running count of position notifications since START.
  ++position_notification_count_;

  RequestNextClockRecoveryUpdate();
}

void AudioDriver::RequestNextClockRecoveryUpdate() {
  FX_CHECK(clock_domain_ != Clock::kMonotonicDomain);

  ring_buffer_fidl_->WatchClockRecoveryPositionInfo(
      [this](fuchsia::hardware::audio::RingBufferPositionInfo info) { ClockRecoveryUpdate(info); });
  // No need for a driver command timeout: this is a "hanging get".
}

zx_status_t AudioDriver::Start() {
  TRACE_DURATION("audio", "AudioDriver::Start");
  // TODO(fxbug.dev/13665): Figure out a better way to assert this!
  OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());

  // In order to start, we must be in the Configured state.
  //
  // Note: Attempting to start while already started is considered an error because (since we are
  // already started) we will never deliver the OnDriverStartComplete callback. It would be
  // confusing to call it directly from here -- before the user's call to Start even returned.
  if (state_ != State::Configured) {
    FX_LOGS(ERROR) << "Bad state while attempting start (state = " << static_cast<uint32_t>(state_)
                   << ")";
    return ZX_ERR_BAD_STATE;
  }

  // Change state, setup our command timeout and we are finished.
  state_ = State::Starting;
  SetCommandTimeout(kDefaultShortCmdTimeout, "RingBuffer::Start");

  ring_buffer_fidl_->Start([this](int64_t start_time) {
    if constexpr (kLogAudioDriverCallbacks) {
      FX_LOGS(INFO) << "AudioDriver::ring_buffer_fidl::Start callback";
    }

    OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());
    if (state_ != State::Starting) {
      FX_LOGS(ERROR) << "Received unexpected start response while in state "
                     << static_cast<uint32_t>(state_);
      return;
    }

    mono_start_time_ = zx::time(start_time);
    ref_start_time_ = reference_clock()->ReferenceTimeFromMonotonicTime(mono_start_time_);

    auto format = GetFormat();
    auto frac_fps = TimelineRate(Fixed(format->frames_per_second()).raw_value(), zx::sec(1).get());

    auto raw_internal_delay_frames = Fixed(internal_delay_frames_).raw_value();
    if (owner_->is_output()) {
      // Abstractly, we can think of the hardware buffer as an infinitely long sequence of frames,
      // where the hardware maintains three pointers into this sequence:
      //
      //      |<--- external delay --->|<--- internal delay --->|
      //      +-+----------------------+-+----------------------+-+
      //  ... |P|                      |F|                      |W| ...
      //      +-+----------------------+-+----------------------+-+
      //
      // We use a timeline to understand the relationships (specifically the offsets) between three
      // specific steps in the playback process, at any specific instant in time.
      //
      // At that moment:
      // W refers to the frame that is about to be consumed by the device.
      // F refers to the frame that just exited the device's internal pipeline.
      // P refers to the frame being presented to the speaker.
      //
      // When a frame is consumed by an audio output device, it is DMA'ed from the ring buffer and
      // enters a device-internal pipeline. The pipeline generally starts with a hardware FIFO to
      // receive DMA transfers; it may apply hardware-based audio processing; and it terminates
      // at an "interconnect" (e.g. integrated DAC, external digital port, network interface).
      // Assuming a non-zero internal delay, W is a higher ("later") frame number than F or P.
      //
      // For built-in outputs where the pipeline ends at a DAC, external delay would be zero. For
      // other device types, "device-external" steps may exist beyond the HDMI or S/PDIF port, or
      // may occur after frames are emitted over Bluetooth.
      // Assuming a non-zero external delay, F is a higher frame number (it is "later") than P.
      //
      // In the above diagram, a specific frame will move right-to-left over time:
      // - "Internal delay" is the time needed for a frame to move from position W to position F;
      // - "External delay" is the time needed for a frame to move from position F to position P.
      //
      // At ref_start_time_, we define the frame pointed to by F as "0". As time advances by one
      // frame, each pointer shifts to the right by one frame. We define functions to locate W and P
      // for any given time T:
      //
      //   ref_time_to_frac_presentation_frame(T) = P
      //   ref_time_to_frac_safe_write_frame(T) = W
      //
      // It stands to reason that clients must write frames to the ring buffer BEFORE the hardware
      // reads/processes/outputs them. Compared to frames being actively processed, client frames
      // are later or "younger"; on our timeline, playback clients must stay "fully to the right".
      // W is the lowest-numbered (soonest to be presented) frame that clients may write to the
      // buffer, aka the "first safe" write position. (Device has already consumed frames beyond W.)
      ref_time_to_frac_presentation_frame_ = TimelineFunction(
          0,                                          // When playback begins, F is 0; we define
          (ref_start_time_ + external_delay_).get(),  // P as occurring external_delay later.
          frac_fps                                    // Converts fractional frames to seconds.
      );
      ref_time_to_frac_safe_read_or_write_frame_ = TimelineFunction(
          raw_internal_delay_frames,  // F is 0, and first safe frame W is internal_delay more,
          ref_start_time_.get(),      // at the moment that playback begins.
          frac_fps                    // Converts fractional frames to seconds.
      );

      if constexpr (kLogDriverDelayProperties) {
        FX_LOGS(INFO)
            << "Setting OUTPUT ref_time_to_frac_presentation_frame_, based on 0 first frac-frame, "
            << ref_start_time_.get() / 1000 << "-usec ref_start_time + " << std::setw(5)
            << external_delay_.to_usecs() << "-usec external delay, and frac-fps "
            << frac_fps.subject_delta() << "/" << frac_fps.reference_delta();
        FX_LOGS(INFO) << "Setting ref_time_to_frac_safe_read_or_write_frame_, based on "
                      << raw_internal_delay_frames << " first frac-frame, "
                      << ref_start_time_.get() / 1000 << "-usec ref_start_time, and frac-fps "
                      << frac_fps.subject_delta() << "/" << frac_fps.reference_delta();
      }
    } else {
      // The capture buffer works in a similar way, with three analogous pointers:
      //
      //        |<--- internal delay --->|<--- external delay --->|
      //      +-+----------------------+-+----------------------+-+
      //  ... |R|                      |F|                      |C| ...
      //      +-+----------------------+-+----------------------+-+
      //
      // At a specific moment in time:
      // R refers to the frame just written to the ring buffer, newly available to capture clients.
      // F refers to the frame just emitted by the interconnect, entering any internal pipeline.
      // C refers to the frame currently being captured by the microphone.
      //
      // As above, F is a larger frame number than R, because F is not yet available in the ring
      // buffer (it will only be available after an additional "internal delay" has elapsed).
      // As with playback, in our diagram any specific frame will move right-to-left over time:
      // - "External delay" is the time needed for a frame to move from position C to position F;
      // - "Internal delay" is the time needed for a frame to move from position F to position R.
      //
      // Just as with playback, we define F to a value of 0 at ref_start_time_. Pointers shift to
      // the right as time advances, and we define functions to locate C and R:
      //
      //   ref_time_to_frac_presentation_frame(T) = C         more accurately "frac_capture_time"
      //   ref_time_to_frac_safe_read_frame(T) = R
      //
      // It stands to reason that a client reads a frame from the ring buffer only AFTER a device
      // captures and writes it; frames visible to the client are older than those being currently
      // captured. Thus capture clients must stay "fully to the left", on our timeline.
      // R is the highest-numbered (most recently captured) frame that a client may be read from the
      // buffer, the "last safe" read position. (Device is still producing the frames beyond R.)
      ref_time_to_frac_presentation_frame_ = TimelineFunction(
          0,                                          // When capture begins, F is 0; C is defined
          (ref_start_time_ - external_delay_).get(),  // as having occurred external_delay earlier.
          frac_fps                                    // Converts fractional frames to seconds.
      );
      ref_time_to_frac_safe_read_or_write_frame_ = TimelineFunction(
          -raw_internal_delay_frames,  // We define R as internal_delay behind (less than) F,
          ref_start_time_.get(),       // which is 0 at the moment that capture begins.
          frac_fps                     // Converts fractional frames to seconds.
      );

      if constexpr (kLogDriverDelayProperties) {
        FX_LOGS(INFO)
            << "Setting INPUT ref_time_to_frac_presentation_frame_, based on 0 first frac-frame, "
            << ref_start_time_.get() / 1000 << "-usec ref_start_time - " << std::setw(5)
            << external_delay_.to_usecs() << "-usec external delay, and frac-fps "
            << frac_fps.subject_delta() << "/" << frac_fps.reference_delta();
        FX_LOGS(INFO) << "Setting ref_time_to_frac_safe_read_or_write_frame_, based on "
                      << -raw_internal_delay_frames << " first frac-frame, "
                      << ref_start_time_.get() / 1000 << "-usec ref_start_time, and frac-fps "
                      << frac_fps.subject_delta() << "/" << frac_fps.reference_delta();
      }
    }

    versioned_ref_time_to_frac_presentation_frame_->Update(ref_time_to_frac_presentation_frame_);
    if (clock_domain_ != Clock::kMonotonicDomain) {
      auto frac_frame_to_ref_time = ref_time_to_frac_presentation_frame_.Inverse();
      auto bytes_to_frac_frames = TimelineFunction(0, 0, frac_frames_per_byte_);
      auto bytes_to_ref_time = frac_frame_to_ref_time * bytes_to_frac_frames;

      FX_CHECK(recovered_clock_);
      recovered_clock_->Reset(mono_start_time_, bytes_to_ref_time);
    }

    // We are now Started. Let our owner know about this important milestone.
    state_ = State::Started;
    ClearCommandTimeout();
    owner_->OnDriverStartComplete();
  });
  return ZX_OK;
}

zx_status_t AudioDriver::Stop() {
  TRACE_DURATION("audio", "AudioDriver::Stop");
  // TODO(fxbug.dev/13665): Figure out a better way to assert this!
  OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());

  // In order to stop, we must be in the Started state.
  // TODO(fxbug.dev/13668): make Stop idempotent. Allow Stop when Configured/Stopping; disallow if
  // Shutdown; consider what to do if Uninitialized/MissingDriverInfo/Unconfigured/Configuring. Most
  // importantly, if driver is Starting, queue the request until Start completes (as we cannot
  // cancel driver commands). Finally, handle multiple Stop calls to be in-flight concurrently.
  if (state_ != State::Started) {
    FX_LOGS(ERROR) << "Bad state while attempting stop (state = " << static_cast<uint32_t>(state_)
                   << ")";
    return ZX_ERR_BAD_STATE;
  }

  // Invalidate our timeline transformation here. To outside observers, we are now stopped.
  versioned_ref_time_to_frac_presentation_frame_->Update({});

  // We are now in the Stopping state.
  state_ = State::Stopping;
  SetCommandTimeout(kDefaultShortCmdTimeout, "RingBuffer::Stop");

  ring_buffer_fidl_->Stop([this]() {
    if constexpr (kLogAudioDriverCallbacks) {
      FX_LOGS(INFO) << "AudioDriver::ring_buffer_fidl::Stop callback";
    }

    OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());
    // We are now stopped and in Configured state. Let our owner know about this important
    // milestone.
    state_ = State::Configured;
    ClearCommandTimeout();
    owner_->OnDriverStopComplete();
  });

  return ZX_OK;
}

zx_status_t AudioDriver::SetPlugDetectEnabled(bool enabled) {
  TRACE_DURATION("audio", "AudioDriver::SetPlugDetectEnabled");

  // This method is a no-op since under the FIDL API plug detect is always enabled if supported.
  return ZX_OK;
}

void AudioDriver::ShutdownSelf(const char* reason, zx_status_t status) {
  TRACE_DURATION("audio", "AudioDriver::ShutdownSelf");
  if (state_ == State::Shutdown) {
    return;
  }

  // Always log: this should occur rarely, hence it should not spam.
  FX_PLOGS(INFO, status) << (owner_->is_input() ? " Input" : "Output") << " shutting down '"
                         << reason << "'";

  // Our owner will call our Cleanup function within this call.
  owner_->ShutdownSelf();
  state_ = State::Shutdown;
}

// Start a timer for the driver command described by 'cmd_tag'.
void AudioDriver::SetCommandTimeout(const zx::duration& deadline, const std::string& cmd_tag) {
  TRACE_DURATION("audio", "AudioDriver::SetCommandTimeout");
  configuration_deadline_ = async::Now(owner_->mix_domain().dispatcher()) + deadline;
  SetupCommandTimeout(cmd_tag);
}

void AudioDriver::ClearCommandTimeout() {
  TRACE_DURATION("audio", "AudioDriver::ClearCommandTimeout");
  configuration_deadline_ = zx::time::infinite();
  SetupCommandTimeout("");
}

void AudioDriver::SetupCommandTimeout(const std::string& cmd_tag) {
  TRACE_DURATION("audio", "AudioDriver::SetupCommandTimeout");

  // If we have received a late response, report it now.
  if (driver_last_timeout_ != zx::time::infinite()) {
    auto delay = async::Now(owner_->mix_domain().dispatcher()) - driver_last_timeout_;
    driver_last_timeout_ = zx::time::infinite();
    FX_DCHECK(timeout_handler_);
    timeout_handler_(delay, driver_last_cmd_tag_);
  }

  if (cmd_timeout_.last_deadline() != configuration_deadline_) {
    if (configuration_deadline_ != zx::time::infinite()) {
      driver_last_cmd_tag_ = cmd_tag;
      cmd_timeout_.PostForTime(owner_->mix_domain().dispatcher(), configuration_deadline_);
    } else {
      driver_last_cmd_tag_ = "";
      cmd_timeout_.Cancel();
    }
  }
}

void AudioDriver::ReportPlugStateChange(bool plugged, zx::time plug_time) {
  TRACE_DURATION("audio", "AudioDriver::ReportPlugStateChange");
  {
    std::lock_guard<std::mutex> lock(plugged_lock_);
    plugged_ = plugged;
    plug_time_ = plug_time;
  }

  // Under the FIDL API plug detect is always enabled.
  owner_->OnDriverPlugStateChange(plugged, plug_time);
}

zx_status_t AudioDriver::OnDriverInfoFetched(uint32_t info) {
  TRACE_DURATION("audio", "AudioDriver::OnDriverInfoFetched");
  // We should never fetch the same info twice.
  if (fetched_driver_info_ & info) {
    ShutdownSelf("Duplicate driver info fetch\n", ZX_ERR_BAD_STATE);
    return ZX_ERR_BAD_STATE;
  }

  // Record the new piece of info we just fetched.
  FX_DCHECK(state_ == State::MissingDriverInfo);
  fetched_driver_info_ |= info;

  // Have we finished fetching our initial driver info? If so, cancel the timeout, transition to
  // Unconfigured state, and let our owner know that we have finished.
  if ((fetched_driver_info_ & kDriverInfoHasAll) == kDriverInfoHasAll) {
    // Now that we have our clock domain, we can establish our audio device clock
    SetUpClocks();

    state_ = State::Unconfigured;
    ClearCommandTimeout();
    owner_->OnDriverInfoFetched();
  }

  return ZX_OK;
}

void AudioDriver::SetUpClocks() {
  if (clock_domain_ == Clock::kMonotonicDomain) {
    // If in the monotonic domain, we'll fall back to a non-adjustable clone of CLOCK_MONOTONIC.
    audio_clock_ = owner_->clock_factory()->CreateDeviceFixed(audio::clock::CloneOfMonotonic(),
                                                              Clock::kMonotonicDomain);
    recovered_clock_ = nullptr;
    return;
  }

  // This clock begins as a clone of MONOTONIC, but because the hardware is NOT in the monotonic
  // clock domain, this clock must eventually diverge. We tune this clock based on notifications
  // provided by the audio driver, which correlate DMA position with CLOCK_MONOTONIC time.
  // TODO(fxbug.dev/60027): Recovered clocks should be per-domain not per-driver.
  auto backing_clock = owner_->clock_factory()->CreateDeviceAdjustable(
      audio::clock::AdjustableCloneOfMonotonic(), clock_domain_);

  // TODO(fxbug.dev/46648): If this clock domain is discovered to be hardware-tunable, we should
  // support a mode where the RecoveredClock is optionally recovered OR tuned depending on how it
  // is used in the mix graph.
  recovered_clock_ = RecoveredClock::Create(
      fxl::StringPrintf("recovered_clock_for_%s",
                        (owner_->is_output() ? "output_device" : "input_device")),
      std::move(backing_clock), kPidFactorsClockChasesDevice);

  // Expose the recovered clock as our reference clock.
  audio_clock_ = recovered_clock_;
}

zx_status_t AudioDriver::SetGain(const AudioDeviceSettings::GainState& gain_state,
                                 audio_set_gain_flags_t set_flags) {
  // We ignore set_flags since the FIDL API requires updates to all field of
  // fuchsia::hardware::audio::GainState.
  return SetGain(gain_state);
}

zx_status_t AudioDriver::SetGain(const AudioDeviceSettings::GainState& gain_state) {
  TRACE_DURATION("audio", "AudioDriver::SetGain");
  fuchsia::hardware::audio::GainState gain_state2 = {};
  if (gain_state.muted) {
    gain_state2.set_muted(true);
  }
  if (gain_state.agc_enabled) {
    gain_state2.set_agc_enabled(true);
  }
  gain_state2.set_gain_db(gain_state.gain_db);
  if constexpr (kLogSetDeviceGainMuteActions) {
    FX_LOGS(INFO) << "AudioDriver(" << (owner_->is_output() ? "output" : "input")
                  << "): StreamConfig/SetGain(" << gain_state.gain_db
                  << (gain_state.muted ? ", MUTED" : "") << (gain_state.agc_enabled ? ", AGC" : "")
                  << ")";
  }
  stream_config_fidl_->SetGain(std::move(gain_state2));
  // No need for a driver command timeout: there is no reply to this FIDL message.
  return ZX_OK;
}

zx_status_t AudioDriver::SelectBestFormat(uint32_t* frames_per_second_inout,
                                          uint32_t* channels_inout,
                                          fuchsia::media::AudioSampleFormat* sample_format_inout) {
  return media::audio::SelectBestFormat(formats_, frames_per_second_inout, channels_inout,
                                        sample_format_inout);
}

void AudioDriver::DriverCommandTimedOut() {
  FX_LOGS(WARNING) << "Unexpected driver timeout: '" << driver_last_cmd_tag_ << "'";
  driver_last_timeout_ = async::Now(owner_->mix_domain().dispatcher());
}

zx_status_t AudioDriver::SetActiveChannels(uint64_t chan_bit_mask) {
  OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());

  if (state_ != State::Started) {
    FX_LOGS(ERROR) << "Unexpected SetActiveChannels request while in state "
                   << static_cast<uint32_t>(state_);
    return ZX_ERR_BAD_STATE;
  }

  if (set_active_channels_err_ != ZX_OK) {
    if constexpr (kLogSetActiveChannelsCalls) {
      FX_LOGS(INFO) << "ring_buffer_fidl->SetActiveChannels(0x" << std::hex << chan_bit_mask
                    << ") NOT called by AudioDriver because of previous set_active_channels_err_ "
                    << std::dec << set_active_channels_err_;
    }
    return set_active_channels_err_;
  }

  if constexpr (kLogSetActiveChannelsCalls) {
    FX_LOGS(INFO) << "ring_buffer_fidl->SetActiveChannels(0x" << std::hex << chan_bit_mask
                  << ") called by AudioDriver";
  }

  SetCommandTimeout(kDefaultLongCmdTimeout, "RingBuffer::SetActiveChannels");

  ring_buffer_fidl_->SetActiveChannels(
      chan_bit_mask,
      [this, chan_bit_mask](fuchsia::hardware::audio::RingBuffer_SetActiveChannels_Result result) {
        OBTAIN_EXECUTION_DOMAIN_TOKEN(token, &owner_->mix_domain());

        ClearCommandTimeout();

        if (result.is_err()) {
          set_active_channels_err_ = result.err();
          FX_LOGS(WARNING) << "ring_buffer_fidl->SetActiveChannels(0x" << std::hex << chan_bit_mask
                           << ") received error " << std::dec << set_active_channels_err_;
          return;
        }
        int64_t set_active_channels_time = result.response().set_time;

        if constexpr (kLogSetActiveChannelsActions) {
          FX_LOGS(INFO) << "ring_buffer_fidl->SetActiveChannels(0x" << std::hex << chan_bit_mask
                        << ") received callback with set_time " << std::dec
                        << set_active_channels_time;
        } else {
          (void)chan_bit_mask;  // avoid "unused lambda capture" compiler complaint
        }

        // TODO(fxbug.dev/82423): assuming this might change the clients' minimum lead time, here we
        // should potentially kick off a notification -- including the set_active_channels_time.
      });

  return ZX_OK;
}

Reporter::AudioDriverInfo AudioDriver::info_for_reporter() const {
  return {
      .manufacturer_name = manufacturer_name(),
      .product_name = product_name(),
      .external_delay = external_delay(),
      .internal_delay = internal_delay(),
      .internal_delay_frames = internal_delay_frames(),
      .format = GetFormat(),
  };
}

}  // namespace media::audio
