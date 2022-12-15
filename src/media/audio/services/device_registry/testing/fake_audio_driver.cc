// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/testing/fake_audio_driver.h"

#include <fuchsia/hardware/audio/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fidl/cpp/unified_messaging_declarations.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fpromise/result.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>
#include <zircon/system/public/zircon/errors.h>

#include <audio-proto-utils/format-utils.h>
#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/logging.h"

namespace media_audio {

FakeAudioDriver::FakeAudioDriver(zx::channel server_end, zx::channel client_end,
                                 async_dispatcher_t* dispatcher)
    : dispatcher_(dispatcher),
      stream_config_server_end_(std::move(server_end)),
      stream_config_client_end_(std::move(client_end)) {
  ADR_LOG_OBJECT(kLogFakeAudioDriver || kLogObjectLifetimes);

  SetDefaultFormats();
}

FakeAudioDriver::~FakeAudioDriver() { ADR_LOG_OBJECT(kLogFakeAudioDriver || kLogObjectLifetimes); }

zx::channel FakeAudioDriver::Enable() {
  ADR_LOG_OBJECT(kLogFakeAudioDriver);

  EXPECT_FALSE(stream_config_binding_);
  stream_config_binding_.emplace(this, std::move(stream_config_server_end_), dispatcher_);
  EXPECT_TRUE(stream_config_binding_->is_bound());

  return std::move(stream_config_client_end_);
}

void FakeAudioDriver::DropStreamConfig() {
  ADR_LOG_OBJECT(kLogFakeAudioDriver);

  stream_config_binding_->Close(ZX_ERR_PEER_CLOSED);
}

void FakeAudioDriver::InjectGainChange(fuchsia_hardware_audio::GainState gain_state) {
  fuchsia::hardware::audio::GainState new_state;
  gain_state.gain_db() ? new_state.set_gain_db(*gain_state.gain_db()) : new_state;
  gain_state.muted() ? new_state.set_muted(*gain_state.muted()) : new_state;
  gain_state.agc_enabled() ? new_state.set_agc_enabled(*gain_state.agc_enabled()) : new_state;

  SetGain(std::move(new_state));
}

void FakeAudioDriver::InjectPlugChange(bool plugged, zx::time plug_time) {
  if (!plugged_ || (*plugged_ != plugged && plug_time > plug_state_time_)) {
    plugged_ = plugged;
    plug_state_time_ = plug_time;
    plug_has_changed_ = true;

    if (pending_plug_callback_) {
      WatchPlugState(std::move(pending_plug_callback_));
    }
  }
}

void FakeAudioDriver::GetHealthState(
    fuchsia::hardware::audio::StreamConfig::GetHealthStateCallback callback) {
  fuchsia::hardware::audio::HealthState health_state;
  if (healthy_) {
    health_state.set_healthy(*healthy_);
  }
  callback(std::move(health_state));
}

void FakeAudioDriver::GetProperties(
    fuchsia::hardware::audio::StreamConfig::GetPropertiesCallback callback) {
  ADR_LOG_OBJECT(kLogFakeAudioDriver);

  fuchsia::hardware::audio::StreamProperties props = {};

  uid_ ? (void)std::memcpy(props.mutable_unique_id()->data(), uid_->data(), sizeof(uid_))
       : props.clear_unique_id();
  is_input_ ? (void)props.set_is_input(*is_input_) : props.clear_is_input();

  if (can_mute_) {
    props.set_can_mute(*can_mute_);
  }
  if (can_agc_) {
    props.set_can_agc(*can_agc_);
  }
  if (min_gain_db_) {
    props.set_min_gain_db(*min_gain_db_);
  }
  if (max_gain_db_) {
    props.set_max_gain_db(*max_gain_db_);
  }
  if (gain_step_db_) {
    props.set_gain_step_db(*gain_step_db_);
  }
  if (plug_detect_capabilities_) {
    props.set_plug_detect_capabilities(*plug_detect_capabilities_);
  }
  if (manufacturer_) {
    props.set_manufacturer(*manufacturer_);
  }
  if (product_) {
    props.set_product(*product_);
  }
  if (clock_domain_) {
    props.set_clock_domain(*clock_domain_);
  }

  callback(std::move(props));
}

void FakeAudioDriver::GetSupportedFormats(
    fuchsia::hardware::audio::StreamConfig::GetSupportedFormatsCallback callback) {
  ADR_LOG_OBJECT(kLogFakeAudioDriver);

  auto supported_formats_count = channel_sets_.size();
  if (sample_formats_.size() != supported_formats_count ||
      bytes_per_sample_.size() != supported_formats_count ||
      valid_bits_per_sample_.size() != supported_formats_count ||
      frame_rates_.size() != supported_formats_count) {
    FX_LOGS(ERROR) << "Format vectors must be the same size";
  }

  auto supported_formats =
      std::vector<fuchsia::hardware::audio::SupportedFormats>(supported_formats_count);
  for (auto supported_format_idx = 0u; supported_format_idx < supported_formats_count;
       ++supported_format_idx) {
    fuchsia::hardware::audio::SupportedFormats supported_format;
    if (channel_sets_[supported_format_idx] || sample_formats_[supported_format_idx] ||
        bytes_per_sample_[supported_format_idx] || valid_bits_per_sample_[supported_format_idx] ||
        frame_rates_[supported_format_idx]) {
      fuchsia::hardware::audio::PcmSupportedFormats pcm_supported_formats;
      if (channel_sets_[supported_format_idx]) {
        auto channel_sets = std::vector<fuchsia::hardware::audio::ChannelSet>(
            channel_sets_[supported_format_idx]->size());
        for (auto channel_sets_idx = 0u;
             channel_sets_idx < channel_sets_[supported_format_idx]->size(); ++channel_sets_idx) {
          fuchsia::hardware::audio::ChannelSet channel_set;
          if (channel_sets_[supported_format_idx]->at(channel_sets_idx)) {
            auto attribs = std::vector<fuchsia::hardware::audio::ChannelAttributes>(
                channel_sets_[supported_format_idx]->at(channel_sets_idx)->size());
            for (auto attributes_idx = 0u;
                 attributes_idx < channel_sets_[supported_format_idx]->at(channel_sets_idx)->size();
                 ++attributes_idx) {
              if (channel_sets_[supported_format_idx]
                      ->at(channel_sets_idx)
                      ->at(attributes_idx)
                      .has_min_frequency()) {
                attribs[attributes_idx].set_min_frequency(channel_sets_[supported_format_idx]
                                                              ->at(channel_sets_idx)
                                                              ->at(attributes_idx)
                                                              .min_frequency());
              }
              if (channel_sets_[supported_format_idx]
                      ->at(channel_sets_idx)
                      ->at(attributes_idx)
                      .has_max_frequency()) {
                attribs[attributes_idx].set_max_frequency(channel_sets_[supported_format_idx]
                                                              ->at(channel_sets_idx)
                                                              ->at(attributes_idx)
                                                              .max_frequency());
              }
            }
            channel_set.set_attributes(std::move(attribs));
          }
          channel_sets[channel_sets_idx] = std::move(channel_set);
        }
        pcm_supported_formats.set_channel_sets(std::move(channel_sets));
      }
      if (sample_formats_[supported_format_idx]) {
        pcm_supported_formats.set_sample_formats(*sample_formats_[supported_format_idx]);
      }
      if (bytes_per_sample_[supported_format_idx]) {
        pcm_supported_formats.set_bytes_per_sample(*bytes_per_sample_[supported_format_idx]);
      }
      if (valid_bits_per_sample_[supported_format_idx]) {
        pcm_supported_formats.set_valid_bits_per_sample(
            *valid_bits_per_sample_[supported_format_idx]);
      }
      if (frame_rates_[supported_format_idx]) {
        pcm_supported_formats.set_frame_rates(*frame_rates_[supported_format_idx]);
      }
      supported_format.set_pcm_supported_formats(std::move(pcm_supported_formats));
    }
    supported_formats[supported_format_idx] = std::move(supported_format);
  }
  callback(std::move(supported_formats));
}

void FakeAudioDriver::WatchGainState(
    fuchsia::hardware::audio::StreamConfig::WatchGainStateCallback callback) {
  ADR_LOG_OBJECT(kLogFakeAudioDriver);

  if (gain_has_changed_) {
    fuchsia::hardware::audio::GainState gain_state = {};
    if (current_mute_) {
      gain_state.set_muted(*current_mute_);
    }
    if (current_agc_) {
      gain_state.set_agc_enabled(*current_agc_);
    }
    if (current_gain_db_) {
      gain_state.set_gain_db(*current_gain_db_);
    }
    gain_has_changed_ = false;
    callback(std::move(gain_state));
  } else {
    pending_gain_callback_ = std::move(callback);
  }
}

void FakeAudioDriver::WatchPlugState(
    fuchsia::hardware::audio::StreamConfig::WatchPlugStateCallback callback) {
  ADR_LOG_OBJECT(kLogFakeAudioDriver);

  if (plug_has_changed_) {
    fuchsia::hardware::audio::PlugState plug_state = {};
    if (plugged_) {
      plug_state.set_plugged(*plugged_);
    }
    plug_state.set_plug_state_time(plug_state_time_.get());
    plug_has_changed_ = false;
    callback(std::move(plug_state));
  } else {
    pending_plug_callback_ = std::move(callback);
  }
}

// Only generate a WatchGainState notification if this SetGain call represents an actual change.
void FakeAudioDriver::SetGain(fuchsia::hardware::audio::GainState target_state) {
  ADR_LOG_OBJECT(kLogFakeAudioDriver);
  bool gain_notification_needed = false;
  if (target_state.has_gain_db() &&
      (!current_gain_db_ || *current_gain_db_ != target_state.gain_db())) {
    current_gain_db_ = target_state.gain_db();
    gain_notification_needed = true;
  }
  if (target_state.has_muted() && target_state.muted() != current_mute_.value_or(false)) {
    current_mute_ = target_state.muted();
    gain_notification_needed = true;
  }
  if (target_state.has_agc_enabled() &&
      target_state.agc_enabled() != current_agc_.value_or(false)) {
    current_agc_ = target_state.agc_enabled();
    gain_notification_needed = true;
  }
  if (gain_notification_needed) {
    gain_has_changed_ = true;
    if (pending_gain_callback_) {
      WatchGainState(std::move(pending_gain_callback_));
    }
  }
}

// For now, don't do anything with this. No response is needed so this should be OK even if called.
void FakeAudioDriver::CreateRingBuffer(
    fuchsia::hardware::audio::Format format,
    fidl::InterfaceRequest<fuchsia::hardware::audio::RingBuffer> ring_buffer_request) {
  ADR_LOG_OBJECT(kLogFakeAudioDriver);
}

// For now, don't do anything with this. No response is needed so this should be OK even if called.
void FakeAudioDriver::SignalProcessingConnect(
    fidl::InterfaceRequest<fuchsia::hardware::audio::signalprocessing::SignalProcessing>
        signal_processing_request) {
  ADR_LOG_OBJECT(kLogFakeAudioDriver);
}

}  // namespace media_audio
