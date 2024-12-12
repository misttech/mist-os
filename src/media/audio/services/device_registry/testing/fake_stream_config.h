// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_FAKE_STREAM_CONFIG_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_FAKE_STREAM_CONFIG_H_

#include <fuchsia/hardware/audio/cpp/fidl.h>
#include <fuchsia/hardware/audio/signalprocessing/cpp/fidl.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fidl/cpp/wire/unknown_interaction_handler.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/zx/channel.h>
#include <lib/zx/time.h>
#include <lib/zx/vmo.h>

#include <cstring>
#include <optional>
#include <string_view>
#include <vector>

#include "src/media/audio/services/device_registry/basic_types.h"
#include "src/media/audio/services/device_registry/logging.h"

namespace media_audio {

// This driver implements the audio driver interface and is configurable to simulate audio hardware.
class FakeStreamConfig final : public fuchsia::hardware::audio::StreamConfig,
                               public fuchsia::hardware::audio::signalprocessing::SignalProcessing,
                               public fuchsia::hardware::audio::RingBuffer {
  static constexpr bool kLogFakeStreamConfig = false;

 public:
  FakeStreamConfig(zx::channel server_end, zx::channel client_end, async_dispatcher_t* dispatcher);
  ~FakeStreamConfig() override;

  // This returns a fidl::client_end<StreamConfig). The driver will not start serving requests until
  // Enable is called, which is why the construction/Enable separation exists.
  fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig> Enable();
  fzl::VmoMapper AllocateRingBuffer(size_t size);
  void DropStreamConfig();
  void DropRingBuffer();

  void set_stream_unique_id(std::optional<UniqueId> uid) {
    if (uid) {
      std::memcpy(uid_->data(), uid->data(), sizeof(*uid));
    } else {
      uid_.reset();
    }
  }
  void set_is_input(std::optional<bool> is_input) { is_input_ = is_input; }
  std::optional<bool> is_input() const { return is_input_; }
  void set_device_manufacturer(std::optional<std::string> mfgr) { manufacturer_ = std::move(mfgr); }
  void set_device_product(std::optional<std::string> product) { product_ = std::move(product); }
  void set_min_gain_db(std::optional<float> min_gain_db) { min_gain_db_ = min_gain_db; }
  void set_max_gain_db(std::optional<float> max_gain_db) { max_gain_db_ = max_gain_db; }
  void set_gain_step_db(std::optional<float> gain_step_db) { gain_step_db_ = gain_step_db; }
  void set_plug_detect_capabilities(
      std::optional<fuchsia::hardware::audio::PlugDetectCapabilities> plug_detect_capabilities) {
    plug_detect_capabilities_ = plug_detect_capabilities;
  }
  void set_can_agc(std::optional<bool> can_agc) { can_agc_ = can_agc; }
  void set_can_mute(std::optional<bool> can_mute) { can_mute_ = can_mute; }
  void set_clock_domain(std::optional<ClockDomain> clock_domain) { clock_domain_ = clock_domain; }

  void set_formats(fuchsia::hardware::audio::PcmSupportedFormats formats) {
    format_set_ = std::move(formats);
  }

  void clear_formats() {
    channel_sets_.clear();
    sample_formats_.clear();
    bytes_per_sample_.clear();
    valid_bits_per_sample_.clear();
    frame_rates_.clear();
  }
  void set_channel_sets(size_t pcm_format_set_idx, size_t channel_set_idx,
                        std::vector<fuchsia::hardware::audio::ChannelAttributes> attributes) {
    if (pcm_format_set_idx >= channel_sets_.size()) {
      channel_sets_.resize(pcm_format_set_idx + 1);
    }
    if (!channel_sets_[pcm_format_set_idx]) {
      channel_sets_[pcm_format_set_idx] =
          std::vector<std::optional<std::vector<fuchsia::hardware::audio::ChannelAttributes>>>();
    }
    if (channel_set_idx >= channel_sets_[pcm_format_set_idx]->size()) {
      channel_sets_[pcm_format_set_idx]->resize(channel_set_idx + 1);
    }
    if (!channel_sets_[pcm_format_set_idx]) {
      channel_sets_[pcm_format_set_idx]->at(channel_set_idx) =
          std::vector<fuchsia::hardware::audio::ChannelAttributes>();
    }
    channel_sets_[pcm_format_set_idx]->at(channel_set_idx) = std::move(attributes);
  }
  void set_sample_formats(
      size_t pcm_format_set_idx,
      const std::vector<fuchsia::hardware::audio::SampleFormat>& sample_formats) {
    if (pcm_format_set_idx >= sample_formats_.size()) {
      sample_formats_.resize(pcm_format_set_idx + 1);
    }
    sample_formats_[pcm_format_set_idx] = sample_formats;
  }
  void set_bytes_per_sample(size_t pcm_format_set_idx, const std::vector<uint8_t>& bytes) {
    if (pcm_format_set_idx >= bytes_per_sample_.size()) {
      bytes_per_sample_.resize(pcm_format_set_idx + 1);
    }
    bytes_per_sample_[pcm_format_set_idx] = bytes;
  }
  void set_valid_bits_per_sample(size_t pcm_format_set_idx,
                                 const std::vector<uint8_t>& valid_bits) {
    if (pcm_format_set_idx >= valid_bits_per_sample_.size()) {
      valid_bits_per_sample_.resize(pcm_format_set_idx + 1);
    }
    valid_bits_per_sample_[pcm_format_set_idx] = valid_bits;
  }
  void set_frame_rates(size_t pcm_format_set_idx, const std::vector<uint32_t>& rates) {
    if (pcm_format_set_idx >= frame_rates_.size()) {
      frame_rates_.resize(pcm_format_set_idx + 1);
    }
    frame_rates_[pcm_format_set_idx] = rates;
  }

  // By default, we support 2-channel, int16 (all bits valid), 48kHz.
  void SetDefaultFormats() {
    clear_formats();

    std::vector<fuchsia::hardware::audio::ChannelAttributes> attribs_set;
    attribs_set.push_back({});
    attribs_set.push_back({});
    set_channel_sets(0, 0, std::move(attribs_set));

    set_sample_formats(0, {fuchsia::hardware::audio::SampleFormat::PCM_SIGNED});
    set_bytes_per_sample(0, {2});
    set_valid_bits_per_sample(0, {16});
    set_frame_rates(0, {48000});
  }

  void set_health_state(std::optional<bool> healthy) { healthy_ = healthy; }

  // Explicitly trigger a gain or plug change, including notification.
  void InjectGainChange(fuchsia_hardware_audio::GainState gain_state);
  void InjectPluggedAt(zx::time plug_time);
  void InjectUnpluggedAt(zx::time plug_time);

  void set_active_channels_supported(bool supported) { active_channels_supported_ = supported; }
  uint64_t active_channels_bitmask() const { return active_channels_bitmask_; }
  zx::time set_active_channels_completed_at() const { return set_active_channels_completed_at_; }

  // Explicitly trigger a change notification, for the current values of gain/plug/delay.
  void InjectDelayUpdate(std::optional<zx::duration> internal_delay,
                         std::optional<zx::duration> external_delay);
  // Has a change been made, that will generate an immediate response to the next Watch... call?
  bool delay_has_changed() const { return delay_has_changed_; }
  // Is a Watch... callback pending, that will respond to the next "injected" update?
  bool delay_callback_pending() const { return (pending_delay_callback_ != nullptr); }

  zx::time mono_start_time() const { return mono_start_time_; }
  bool started() const { return started_; }

  // The returned optional will be empty if no |CreateRingBuffer| command has been received.
  std::optional<fuchsia::hardware::audio::PcmFormat> selected_format() const {
    return selected_format_;
  }

  void set_turn_on_delay(zx::duration turn_on_delay) { turn_on_delay_ = turn_on_delay; }
  void clear_turn_on_delay() { turn_on_delay_.reset(); }

 private:
  static inline const std::string_view kClassName = "FakeStreamConfig";

  // fuchsia hardware audio StreamConfig Interface
  void GetProperties(fuchsia::hardware::audio::StreamConfig::GetPropertiesCallback callback) final;
  void GetHealthState(
      fuchsia::hardware::audio::StreamConfig::GetHealthStateCallback callback) final;
  void GetSupportedFormats(
      fuchsia::hardware::audio::StreamConfig::GetSupportedFormatsCallback callback) final;
  void WatchGainState(
      fuchsia::hardware::audio::StreamConfig::WatchGainStateCallback callback) final;
  void WatchPlugState(
      fuchsia::hardware::audio::StreamConfig::WatchPlugStateCallback callback) final;
  void SetGain(fuchsia::hardware::audio::GainState target_state) final;
  void CreateRingBuffer(
      fuchsia::hardware::audio::Format format,
      fidl::InterfaceRequest<fuchsia::hardware::audio::RingBuffer> ring_buffer_request) final;
  void SignalProcessingConnect(
      fidl::InterfaceRequest<fuchsia::hardware::audio::signalprocessing::SignalProcessing>
          signal_processing_request) final;

  // fuchsia hardware audio signalprocessing SignalProcessing Interface
  // These won't be called until SignalProcessingConnect is implemented, but we'll be safe.
  void GetElements(GetElementsCallback callback) override {}
  void WatchElementState(ElementId processing_element_id,
                         WatchElementStateCallback callback) override {}
  void GetTopologies(GetTopologiesCallback callback) override {}
  void WatchTopology(WatchTopologyCallback callback) override {}
  void SetElementState(ElementId processing_element_id,
                       fuchsia::hardware::audio::signalprocessing::SettableElementState state,
                       SetElementStateCallback callback) override {}
  void SetTopology(TopologyId topology_id, SetTopologyCallback callback) override {}
  // Can we templatize to discern between SignalProcessing and RingBuffer here?
  void handle_unknown_method(uint64_t ordinal, bool method_has_response) override {
    ADR_WARN_METHOD() << "FakeStreamConfig: unknown method ordinal " << ordinal;
  }

  // fuchsia hardware audio RingBuffer Interface
  void GetProperties(fuchsia::hardware::audio::RingBuffer::GetPropertiesCallback callback) final;
  void WatchClockRecoveryPositionInfo(
      fuchsia::hardware::audio::RingBuffer::WatchClockRecoveryPositionInfoCallback callback) final;
  void GetVmo(uint32_t min_frames, uint32_t clock_recovery_notifications_per_ring,
              fuchsia::hardware::audio::RingBuffer::GetVmoCallback callback) final;
  void Start(fuchsia::hardware::audio::RingBuffer::StartCallback callback) final;
  void Stop(fuchsia::hardware::audio::RingBuffer::StopCallback callback) final;
  void SetActiveChannels(
      uint64_t active_channels_bitmask,
      fuchsia::hardware::audio::RingBuffer::SetActiveChannelsCallback callback) final;
  void WatchDelayInfo(WatchDelayInfoCallback callback) final;
  // handle_unknown_method for fuchsia::hardware::audio::RingBuffer?

  void PositionNotification();
  void SendPositionNotification(zx::time timestamp, uint32_t position);

  std::optional<UniqueId> uid_ = kDefaultUniqueId;
  std::optional<bool> is_input_ = false;
  std::optional<bool> can_mute_ = true;
  std::optional<bool> can_agc_ = true;
  std::optional<float> min_gain_db_ = -90.0f;
  std::optional<float> max_gain_db_ = 10.0f;
  std::optional<float> gain_step_db_ = 1.0f;
  std::optional<fuchsia::hardware::audio::PlugDetectCapabilities> plug_detect_capabilities_ =
      fuchsia::hardware::audio::PlugDetectCapabilities::CAN_ASYNC_NOTIFY;
  std::optional<std::string> manufacturer_ = "fake stream_config device manufacturer";
  std::optional<std::string> product_ = "fake stream_config device product";
  std::optional<ClockDomain> clock_domain_ = fuchsia::hardware::audio::CLOCK_DOMAIN_MONOTONIC;

  std::optional<bool> healthy_ = true;

  std::optional<float> current_gain_db_ = 0.0f;
  std::optional<bool> current_agc_ = false;
  std::optional<bool> current_mute_ = false;

  std::optional<bool> plugged_ = true;
  zx::time plug_state_time_ = zx::time(0);

  // The default values for these five vectors are set by SetDefaultFormats(), in the ctor.
  std::vector<std::optional<
      std::vector<std::optional<std::vector<fuchsia::hardware::audio::ChannelAttributes>>>>>
      channel_sets_;
  std::vector<std::optional<std::vector<fuchsia::hardware::audio::SampleFormat>>> sample_formats_;
  std::vector<std::optional<std::vector<uint8_t>>> bytes_per_sample_;
  std::vector<std::optional<std::vector<uint8_t>>> valid_bits_per_sample_;
  std::vector<std::optional<std::vector<uint32_t>>> frame_rates_;

  static constexpr UniqueId kDefaultUniqueId{
      {0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef,  //
       0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10},
  };

  fuchsia::hardware::audio::PcmSupportedFormats format_set_;
  size_t ring_buffer_size_;
  zx::vmo ring_buffer_;

  std::optional<zx::duration> internal_delay_ = zx::nsec(0);
  std::optional<zx::duration> external_delay_;
  std::optional<bool> needs_cache_flush_or_invalidate_ = true;
  std::optional<zx::duration> turn_on_delay_;
  // An arbitrarily-chosen transfer size -- 3 frames. At 48khz, this is 62.5 usec.
  std::optional<uint32_t> driver_transfer_bytes_ = 12;

  std::optional<fuchsia::hardware::audio::PcmFormat> selected_format_;

  bool active_channels_supported_ = true;
  uint64_t active_channels_bitmask_;
  zx::time set_active_channels_completed_at_{0};

  bool started_ = false;
  zx::time mono_start_time_{0};

  async_dispatcher_t* dispatcher_;
  std::optional<fidl::Binding<fuchsia::hardware::audio::StreamConfig>> stream_config_binding_;
  zx::channel stream_config_server_end_;
  zx::channel stream_config_client_end_;

  std::optional<fidl::Binding<fuchsia::hardware::audio::RingBuffer>> ring_buffer_binding_;
  std::optional<fidl::Binding<fuchsia::hardware::audio::signalprocessing::SignalProcessing>>
      signal_processing_binding_;

  bool position_notification_values_are_set_ = false;
  zx::time position_notify_timestamp_mono_;
  uint32_t position_notify_position_bytes_ = 0;
  std::optional<fuchsia::hardware::audio::RingBuffer::WatchClockRecoveryPositionInfoCallback>
      position_notify_callback_;

  // Always respond to the first hanging-get request.
  bool gain_has_changed_ = true;
  bool plug_has_changed_ = true;
  bool delay_has_changed_ = true;
  fuchsia::hardware::audio::StreamConfig::WatchGainStateCallback pending_gain_callback_ = nullptr;
  fuchsia::hardware::audio::StreamConfig::WatchPlugStateCallback pending_plug_callback_ = nullptr;
  fuchsia::hardware::audio::RingBuffer::WatchDelayInfoCallback pending_delay_callback_ = nullptr;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_FAKE_STREAM_CONFIG_H_
