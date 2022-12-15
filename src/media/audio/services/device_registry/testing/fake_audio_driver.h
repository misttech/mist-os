// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_FAKE_AUDIO_DRIVER_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_FAKE_AUDIO_DRIVER_H_

#include <fidl/fuchsia.hardware.audio/cpp/markers.h>
#include <fuchsia/hardware/audio/cpp/fidl.h>
#include <lib/async/cpp/time.h>
#include <lib/fidl/cpp/binding_set.h>
#include <lib/fidl/cpp/wire/channel.h>
#include <lib/fpromise/result.h>
#include <lib/fzl/vmo-mapper.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/channel.h>
#include <lib/zx/time.h>
#include <lib/zx/vmo.h>

#include <cstring>
#include <optional>
#include <string_view>
#include <typeinfo>
#include <vector>

#include "src/media/audio/services/device_registry/basic_types.h"

namespace media_audio {

class FakeAudioDriver : public fuchsia::hardware::audio::StreamConfig {
  static inline constexpr bool kLogFakeAudioDriver = false;

 public:
  FakeAudioDriver(zx::channel server_end, zx::channel client_end, async_dispatcher_t* dispatcher);
  ~FakeAudioDriver() override;

  // This returns a fidl::client_end<StreamConfig). The driver will not start serving requests until
  // Enable is called, which is why the construction/Enable separation exists.
  zx::channel Enable();
  void DropStreamConfig();

  void set_stream_unique_id(std::optional<UniqueId> uid) {
    if (uid) {
      std::memcpy(uid_->data(), uid->data(), sizeof(uid));
    } else {
      uid = std::nullopt;
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
  void set_sample_formats(size_t pcm_format_set_idx,
                          std::vector<fuchsia::hardware::audio::SampleFormat> sample_formats) {
    if (pcm_format_set_idx >= sample_formats_.size()) {
      sample_formats_.resize(pcm_format_set_idx + 1);
    }
    sample_formats_[pcm_format_set_idx] = sample_formats;
  }
  void set_bytes_per_sample(size_t pcm_format_set_idx, std::vector<uint8_t> bytes) {
    if (pcm_format_set_idx >= bytes_per_sample_.size()) {
      bytes_per_sample_.resize(pcm_format_set_idx + 1);
    }
    bytes_per_sample_[pcm_format_set_idx] = bytes;
  }
  void set_valid_bits_per_sample(size_t pcm_format_set_idx, std::vector<uint8_t> valid_bits) {
    if (pcm_format_set_idx >= valid_bits_per_sample_.size()) {
      valid_bits_per_sample_.resize(pcm_format_set_idx + 1);
    }
    valid_bits_per_sample_[pcm_format_set_idx] = valid_bits;
  }
  void set_frame_rates(size_t pcm_format_set_idx, std::vector<uint32_t> rates) {
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
  void InjectGainChange(fuchsia_hardware_audio::GainState new_state);
  void InjectPlugChange(bool plugged, zx::time plug_time);

 private:
  static inline const std::string_view kClassName = "FakeAudioDriver";

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

  std::optional<UniqueId> uid_ = kDefaultUniqueId;
  std::optional<bool> is_input_ = false;
  std::optional<bool> can_mute_ = true;
  std::optional<bool> can_agc_ = true;
  std::optional<float> min_gain_db_ = -90.0f;
  std::optional<float> max_gain_db_ = 10.0f;
  std::optional<float> gain_step_db_ = 1.0f;
  std::optional<fuchsia::hardware::audio::PlugDetectCapabilities> plug_detect_capabilities_ =
      fuchsia::hardware::audio::PlugDetectCapabilities::CAN_ASYNC_NOTIFY;
  std::optional<std::string> manufacturer_ = "fake_audio_driver device manufacturer";
  std::optional<std::string> product_ = "fake_audio_driver device product";
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

  static inline constexpr UniqueId kDefaultUniqueId{
      {0x01, 0x23, 0x45, 0x67, 0x89, 0xab, 0xcd, 0xef,  //
       0xfe, 0xdc, 0xba, 0x98, 0x76, 0x54, 0x32, 0x10},
  };

  fuchsia::hardware::audio::PcmSupportedFormats format_set_ = {};

  async_dispatcher_t* dispatcher_;
  std::optional<fidl::Binding<fuchsia::hardware::audio::StreamConfig>> stream_config_binding_;
  zx::channel stream_config_server_end_;
  zx::channel stream_config_client_end_;

  // Always respond to the first hanging-get request.
  bool gain_has_changed_ = true;
  bool plug_has_changed_ = true;
  fuchsia::hardware::audio::StreamConfig::WatchGainStateCallback pending_gain_callback_ = nullptr;
  fuchsia::hardware::audio::StreamConfig::WatchPlugStateCallback pending_plug_callback_ = nullptr;
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_TESTING_FAKE_AUDIO_DRIVER_H_
