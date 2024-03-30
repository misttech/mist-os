// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"

#include <fuchsia/hardware/audio/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fidl/cpp/unified_messaging_declarations.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fpromise/result.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>
#include <lib/zx/result.h>
#include <zircon/errors.h>

#include <audio-proto-utils/format-utils.h>
#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/logging.h"

namespace media_audio {

namespace fha = fuchsia::hardware::audio;

FakeStreamConfig::FakeStreamConfig(zx::channel server_end, zx::channel client_end,
                                   async_dispatcher_t* dispatcher)
    : dispatcher_(dispatcher),
      stream_config_server_end_(std::move(server_end)),
      stream_config_client_end_(std::move(client_end)) {
  ADR_LOG_METHOD(kLogFakeStreamConfig || kLogObjectLifetimes);

  SetDefaultFormats();
}

FakeStreamConfig::~FakeStreamConfig() {
  ADR_LOG_METHOD(kLogFakeStreamConfig || kLogObjectLifetimes);
  DropStreamConfig();
}

fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig> FakeStreamConfig::Enable() {
  ADR_LOG_METHOD(kLogFakeStreamConfig);

  EXPECT_FALSE(stream_config_binding_);
  stream_config_binding_.emplace(this, std::move(stream_config_server_end_), dispatcher_);
  EXPECT_TRUE(stream_config_binding_->is_bound());
  stream_config_binding_->set_error_handler([this](zx_status_t status) { DropStreamConfig(); });

  return fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(
      std::move(stream_config_client_end_));
}

void FakeStreamConfig::DropStreamConfig() {
  if (stream_config_binding_.has_value()) {
    ADR_LOG_METHOD(kLogFakeStreamConfig);

    DropRingBuffer();
    stream_config_binding_->Close(ZX_ERR_PEER_CLOSED);
    stream_config_binding_.reset();
  }
}

void FakeStreamConfig::InjectGainChange(fuchsia_hardware_audio::GainState gain_state) {
  ADR_LOG_METHOD(kLogFakeStreamConfig);
  fha::GainState new_state;
  gain_state.gain_db() ? new_state.set_gain_db(*gain_state.gain_db()) : new_state;
  gain_state.muted() ? new_state.set_muted(*gain_state.muted()) : new_state;
  gain_state.agc_enabled() ? new_state.set_agc_enabled(*gain_state.agc_enabled()) : new_state;

  SetGain(std::move(new_state));
}

void FakeStreamConfig::InjectPluggedAt(zx::time plug_time) {
  ADR_LOG_METHOD(kLogFakeStreamConfig) << "(plugged, " << plug_time.get() << ")";
  if (!plugged_ || (!*plugged_ && plug_time > plug_state_time_)) {
    plugged_ = true;
    plug_state_time_ = plug_time;
    plug_has_changed_ = true;

    if (pending_plug_callback_) {
      WatchPlugState(std::move(pending_plug_callback_));
    }
  }
}

void FakeStreamConfig::InjectUnpluggedAt(zx::time plug_time) {
  ADR_LOG_METHOD(kLogFakeStreamConfig) << "(unplugged, " << plug_time.get() << ")";
  if (!plugged_ || (*plugged_ && plug_time > plug_state_time_)) {
    plugged_ = false;
    plug_state_time_ = plug_time;
    plug_has_changed_ = true;

    if (pending_plug_callback_) {
      WatchPlugState(std::move(pending_plug_callback_));
    }
  }
}

void FakeStreamConfig::DropRingBuffer() {
  if (ring_buffer_binding_) {
    ADR_LOG_METHOD(kLogFakeStreamConfig);

    ring_buffer_binding_->Close(ZX_ERR_PEER_CLOSED);
    is_running_ = false;
    mono_start_time_ = zx::time(0);

    delay_has_changed_ = true;
    pending_delay_callback_ = nullptr;
    ring_buffer_binding_.reset();
  }
}

fzl::VmoMapper FakeStreamConfig::AllocateRingBuffer(size_t size) {
  ADR_LOG_METHOD(kLogFakeStreamConfig);

  FX_CHECK(!ring_buffer_) << "Calling AllocateRingBuffer multiple times is not supported";

  ring_buffer_size_ = size;
  fzl::VmoMapper mapper;
  mapper.CreateAndMap(ring_buffer_size_, ZX_VM_PERM_READ | ZX_VM_PERM_WRITE, nullptr,
                      &ring_buffer_);
  return mapper;
}

void FakeStreamConfig::GetHealthState(fha::StreamConfig::GetHealthStateCallback callback) {
  fha::HealthState health_state;
  if (healthy_) {
    health_state.set_healthy(*healthy_);
  }
  callback(std::move(health_state));
}

void FakeStreamConfig::GetProperties(fha::StreamConfig::GetPropertiesCallback callback) {
  ADR_LOG_METHOD(kLogFakeStreamConfig);

  fha::StreamProperties props = {};

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

void FakeStreamConfig::GetSupportedFormats(
    fha::StreamConfig::GetSupportedFormatsCallback callback) {
  ADR_LOG_METHOD(kLogFakeStreamConfig);

  auto ring_buffer_format_sets_count = channel_sets_.size();
  if (sample_formats_.size() != ring_buffer_format_sets_count ||
      bytes_per_sample_.size() != ring_buffer_format_sets_count ||
      valid_bits_per_sample_.size() != ring_buffer_format_sets_count ||
      frame_rates_.size() != ring_buffer_format_sets_count) {
    FX_LOGS(ERROR) << "Format vectors must be the same size";
  }

  auto supported_formats = std::vector<fha::SupportedFormats>(ring_buffer_format_sets_count);
  for (auto supported_format_idx = 0u; supported_format_idx < ring_buffer_format_sets_count;
       ++supported_format_idx) {
    fha::SupportedFormats supported_format;
    if (channel_sets_[supported_format_idx] || sample_formats_[supported_format_idx] ||
        bytes_per_sample_[supported_format_idx] || valid_bits_per_sample_[supported_format_idx] ||
        frame_rates_[supported_format_idx]) {
      fha::PcmSupportedFormats pcm_supported_formats;
      if (channel_sets_[supported_format_idx]) {
        auto channel_sets =
            std::vector<fha::ChannelSet>(channel_sets_[supported_format_idx]->size());
        for (auto channel_sets_idx = 0u;
             channel_sets_idx < channel_sets_[supported_format_idx]->size(); ++channel_sets_idx) {
          fha::ChannelSet channel_set;
          if (channel_sets_[supported_format_idx]->at(channel_sets_idx)) {
            auto attribs = std::vector<fha::ChannelAttributes>(
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

void FakeStreamConfig::WatchGainState(fha::StreamConfig::WatchGainStateCallback callback) {
  ADR_LOG_METHOD(kLogFakeStreamConfig);

  if (gain_has_changed_) {
    fha::GainState gain_state = {};
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

void FakeStreamConfig::WatchPlugState(fha::StreamConfig::WatchPlugStateCallback callback) {
  ADR_LOG_METHOD(kLogFakeStreamConfig);

  if (plug_has_changed_) {
    fha::PlugState plug_state = {};
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
void FakeStreamConfig::SetGain(fha::GainState target_state) {
  ADR_LOG_METHOD(kLogFakeStreamConfig);
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

void FakeStreamConfig::CreateRingBuffer(
    fha::Format format, fidl::InterfaceRequest<fha::RingBuffer> ring_buffer_request) {
  ADR_LOG_METHOD(kLogFakeStreamConfig);

  ring_buffer_binding_.emplace(this, ring_buffer_request.TakeChannel(), dispatcher_);
  ring_buffer_binding_->set_error_handler([this](zx_status_t status) { DropRingBuffer(); });
  selected_format_ = format.pcm_format();

  active_channels_bitmask_ = (1 << selected_format_->number_of_channels) - 1;
  active_channels_set_time_ = zx::clock::get_monotonic();
}

// For now, don't do anything with this. No response is needed so this should be OK even if called.
void FakeStreamConfig::SignalProcessingConnect(
    fidl::InterfaceRequest<fha::signalprocessing::SignalProcessing> signal_processing_request) {
  ADR_LOG_METHOD(kLogFakeStreamConfig);
}

// RingBuffer methods
void FakeStreamConfig::GetProperties(fha::RingBuffer::GetPropertiesCallback callback) {
  ADR_LOG_METHOD(kLogFakeStreamConfig);

  fha::RingBufferProperties props = {};

  if (needs_cache_flush_or_invalidate_.has_value()) {
    props.set_needs_cache_flush_or_invalidate(*needs_cache_flush_or_invalidate_);
  }
  if (turn_on_delay_.has_value()) {
    props.set_turn_on_delay(turn_on_delay_->to_nsecs());
  }
  if (driver_transfer_bytes_.has_value()) {
    props.set_driver_transfer_bytes(*driver_transfer_bytes_);
  }
  callback(std::move(props));
}

void FakeStreamConfig::SendPositionNotification(zx::time timestamp, uint32_t position) {
  ADR_LOG_METHOD(kLogFakeStreamConfig);

  position_notify_timestamp_mono_ = timestamp;
  position_notify_position_bytes_ = position;

  position_notification_values_are_set_ = true;
  if (position_notify_callback_) {
    PositionNotification();
  }
}

void FakeStreamConfig::WatchClockRecoveryPositionInfo(
    fha::RingBuffer::WatchClockRecoveryPositionInfoCallback callback) {
  ADR_LOG_METHOD(kLogFakeStreamConfig);

  position_notify_callback_ = std::move(callback);

  if (position_notification_values_are_set_) {
    PositionNotification();
  }
}

void FakeStreamConfig::PositionNotification() {
  ADR_LOG_METHOD(kLogFakeStreamConfig);

  FX_CHECK(position_notify_callback_);
  FX_CHECK(position_notification_values_are_set_);

  // Real audio drivers can't emit position notifications until started; we shouldn't either
  if (is_running_) {
    // Clear both prerequisites for sending this notification
    position_notification_values_are_set_ = false;
    auto callback = *std::move(position_notify_callback_);
    position_notify_callback_.reset();

    fha::RingBufferPositionInfo info{
        .timestamp = position_notify_timestamp_mono_.get(),
        .position = position_notify_position_bytes_,
    };

    callback(std::move(info));
  }
}

void FakeStreamConfig::GetVmo(uint32_t min_frames, uint32_t clock_recovery_notifications_per_ring,
                              fha::RingBuffer::GetVmoCallback callback) {
  ADR_LOG_METHOD(kLogFakeStreamConfig);

  // This should be true since it's set as part of creating the channel that's carrying these
  // messages.
  FX_CHECK(selected_format_);

  if (!ring_buffer_) {
    // If we haven't created a ring buffer, we'll just drop this request.
    FX_LOGS(ERROR) << "GetVmo will not respond because ring_buffer_ is not yet set";
    return;
  }
  FX_CHECK(ring_buffer_);

  // Dup our ring buffer VMO to send over the channel.
  zx::vmo dup;
  FX_CHECK(ring_buffer_.duplicate(ZX_RIGHT_SAME_RIGHTS, &dup) == ZX_OK);

  // Compute the buffer size in frames.
  auto frame_size = selected_format_->number_of_channels * selected_format_->bytes_per_sample;
  auto ring_buffer_frames = ring_buffer_size_ / frame_size;

  fha::RingBuffer_GetVmo_Result result = {};
  fha::RingBuffer_GetVmo_Response response = {};
  response.num_frames = static_cast<uint32_t>(ring_buffer_frames);
  response.ring_buffer = std::move(dup);
  result.set_response(std::move(response));
  callback(std::move(result));
}

void FakeStreamConfig::Start(fha::RingBuffer::StartCallback callback) {
  ADR_LOG_METHOD(kLogFakeStreamConfig);

  EXPECT_TRUE(!is_running_);
  mono_start_time_ = zx::clock::get_monotonic();
  is_running_ = true;

  callback(mono_start_time_.get());
}

void FakeStreamConfig::Stop(fha::RingBuffer::StopCallback callback) {
  ADR_LOG_METHOD(kLogFakeStreamConfig);

  EXPECT_TRUE(is_running_);
  is_running_ = false;

  position_notify_callback_.reset();
  position_notification_values_are_set_ = false;

  callback();
}

void FakeStreamConfig::SetActiveChannels(uint64_t active_channels_bitmask,
                                         fha::RingBuffer::SetActiveChannelsCallback callback) {
  ADR_LOG_METHOD(kLogFakeStreamConfig);

  if (active_channels_supported_) {
    if (active_channels_bitmask != active_channels_bitmask_) {
      active_channels_bitmask_ = active_channels_bitmask;
      active_channels_set_time_ = zx::clock::get_monotonic();
    }
    FX_LOGS(DEBUG) << __FUNCTION__ << " active_channels_bitmask_ 0x" << std::hex
                   << active_channels_bitmask_ << ", active_channels_supported_ "
                   << active_channels_supported_;
    callback(fpromise::ok(active_channels_set_time_.get()));
  } else {
    callback(fpromise::error(ZX_ERR_NOT_SUPPORTED));
  }
}

void FakeStreamConfig::WatchDelayInfo(WatchDelayInfoCallback callback) {
  ADR_LOG_METHOD(kLogFakeStreamConfig);

  if (delay_has_changed_) {
    fha::DelayInfo delay_info = {};

    if (internal_delay_) {
      delay_info.set_internal_delay(internal_delay_->to_nsecs());
    }
    if (external_delay_) {
      delay_info.set_external_delay(external_delay_->to_nsecs());
    }

    delay_has_changed_ = false;
    callback(std::move(delay_info));
  } else {
    pending_delay_callback_ = std::move(callback);
  }
}

// Trigger a delay-changed event, based on current internal_delay_/external_delay_ values.
void FakeStreamConfig::InjectDelayUpdate(std::optional<zx::duration> internal_delay,
                                         std::optional<zx::duration> external_delay) {
  internal_delay_ = internal_delay;
  external_delay_ = external_delay;

  delay_has_changed_ = true;
  if (pending_delay_callback_) {
    WatchDelayInfo(std::move(pending_delay_callback_));
  }
}

}  // namespace media_audio
