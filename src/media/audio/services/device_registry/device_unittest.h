// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_UNITTEST_H_
#define SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_UNITTEST_H_

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/fidl/cpp/client.h>
#include <lib/fidl/cpp/unified_messaging_declarations.h>
#include <lib/fidl/cpp/wire/status.h>
#include <zircon/compiler.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <memory>
#include <sstream>

#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "src/media/audio/lib/clock/clock.h"
#include "src/media/audio/services/device_registry/control_notify.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/testing/fake_codec.h"
#include "src/media/audio/services/device_registry/testing/fake_device_presence_watcher.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"

namespace media_audio {

// Test class to verify the driver initialization/configuration sequence.
class DeviceTestBase : public gtest::TestLoopFixture {
 public:
  void SetUp() override {
    notify_ = std::make_shared<NotifyStub>(*this);
    fake_device_presence_watcher_ = std::make_shared<FakeDevicePresenceWatcher>();
  }
  void TearDown() override { fake_device_presence_watcher_.reset(); }

 protected:
  static fuchsia_audio_device::Info GetDeviceInfo(const std::shared_ptr<Device>& device) {
    return *device->info();
  }

  static bool HasError(const std::shared_ptr<Device>& device) {
    return device->state_ == Device::State::Error;
  }
  static bool InInitializedState(const std::shared_ptr<Device>& device) {
    return device->state_ == Device::State::DeviceInitialized;
  }
  static bool IsControlled(const std::shared_ptr<Device>& device) {
    return (device->GetControlNotify() != nullptr);
  }

  class NotifyStub : public std::enable_shared_from_this<NotifyStub>, public ControlNotify {
   public:
    explicit NotifyStub(DeviceTestBase& parent) : parent_(parent) {}
    virtual ~NotifyStub() = default;

    bool AddObserver(const std::shared_ptr<Device>& device) {
      return device->AddObserver(shared_from_this());
    }
    bool SetControl(const std::shared_ptr<Device>& device) {
      return device->SetControl(shared_from_this());
    }
    static bool DropControl(const std::shared_ptr<Device>& device) { return device->DropControl(); }

    // ObserverNotify
    //
    void DeviceIsRemoved() final { FX_LOGS(INFO) << __func__; }
    void DeviceHasError() final { FX_LOGS(INFO) << __func__; }
    void GainStateChanged(const fuchsia_audio_device::GainState& new_gain_state) final {
      gain_state_ = new_gain_state;
    }
    void PlugStateChanged(const fuchsia_audio_device::PlugState& new_plug_state,
                          zx::time plug_change_time) final {
      plug_state_ = std::make_pair(new_plug_state, plug_change_time);
    }

    // ControlNotify
    //
    void DeviceDroppedRingBuffer() final { FX_LOGS(INFO) << __func__; }
    void DelayInfoChanged(const fuchsia_audio_device::DelayInfo& new_delay_info) final {
      FX_LOGS(INFO) << __func__;
      delay_info_ = new_delay_info;
    }
    void DaiFormatChanged(
        const std::optional<fuchsia_hardware_audio::DaiFormat>& dai_format,
        const std::optional<fuchsia_hardware_audio::CodecFormatInfo>& codec_format_info) final {
      dai_format_error_.reset();

      if (dai_format.has_value()) {
        LogDaiFormat(dai_format);
        LogCodecFormatInfo(codec_format_info);
        FX_DCHECK(codec_format_info.has_value());
        dai_format_ = *dai_format;
        codec_format_info_ = codec_format_info;
      } else {
        dai_format_.reset();
        codec_format_info_.reset();
      }
    }
    void DaiFormatNotSet(const fuchsia_hardware_audio::DaiFormat& dai_format,
                         zx_status_t driver_error) final {
      FX_LOGS(INFO) << __func__ << "(error " << std::hex << driver_error << ")";
      dai_format_error_ = driver_error;
    }

    void CodecStarted(const zx::time& start_time) final {
      FX_LOGS(INFO) << __func__ << "(" << start_time.get() << ")";
      codec_start_failed_ = false;
      codec_start_time_ = start_time;
      codec_stop_time_.reset();
    }
    void CodecNotStarted() final {
      FX_LOGS(INFO) << __func__;
      codec_start_failed_ = true;
    }
    void CodecStopped(const zx::time& stop_time) final {
      FX_LOGS(INFO) << __func__ << "(" << stop_time.get() << ")";
      codec_stop_failed_ = false;
      codec_stop_time_ = stop_time;
      codec_start_time_.reset();
    }
    void CodecNotStopped() final { codec_stop_failed_ = true; }

    bool codec_is_started() {
      FX_CHECK(codec_start_time_.has_value() != codec_stop_time_.has_value());
      return codec_start_time_.has_value();
    }
    bool codec_is_stopped() {
      FX_CHECK(codec_start_time_.has_value() != codec_stop_time_.has_value());
      return codec_stop_time_.has_value();
    }
    // For testing purposes, reset internal state so we detect new Notify calls (including errors).
    void clear_dai_format() {
      dai_format_.reset();
      codec_format_info_.reset();
      dai_format_error_.reset();
    }
    void clear_codec_start_stop() {
      codec_start_time_.reset();
      codec_stop_time_ = zx::time::infinite_past();
      codec_start_failed_ = false;
      codec_stop_failed_ = false;
    }

    const std::optional<fuchsia_audio_device::GainState>& gain_state() const { return gain_state_; }
    const std::optional<std::pair<fuchsia_audio_device::PlugState, zx::time>>& plug_state() const {
      return plug_state_;
    }
    const std::optional<fuchsia_audio_device::DelayInfo>& delay_info() const { return delay_info_; }
    std::optional<fuchsia_audio_device::GainState>& gain_state() { return gain_state_; }
    std::optional<std::pair<fuchsia_audio_device::PlugState, zx::time>>& plug_state() {
      return plug_state_;
    }
    std::optional<fuchsia_audio_device::DelayInfo>& delay_info() { return delay_info_; }

    std::optional<fuchsia_hardware_audio::DaiFormat>& dai_format() { return dai_format_; }
    std::optional<zx_status_t> dai_format_error() const { return dai_format_error_; }

    std::optional<fuchsia_hardware_audio::CodecFormatInfo>& codec_format_info() {
      return codec_format_info_;
    }
    std::optional<zx::time>& codec_start_time() { return codec_start_time_; }
    bool codec_start_failed() const { return codec_start_failed_; }
    std::optional<zx::time>& codec_stop_time() { return codec_stop_time_; }
    bool codec_stop_failed() const { return codec_stop_failed_; }

   private:
    [[maybe_unused]] DeviceTestBase& parent_;
    std::optional<fuchsia_audio_device::GainState> gain_state_;
    std::optional<std::pair<fuchsia_audio_device::PlugState, zx::time>> plug_state_;
    std::optional<fuchsia_audio_device::DelayInfo> delay_info_;

    std::optional<fuchsia_hardware_audio::DaiFormat> dai_format_;
    std::optional<fuchsia_hardware_audio::CodecFormatInfo> codec_format_info_;
    std::optional<zx::time> codec_start_time_;
    std::optional<zx::time> codec_stop_time_{zx::time::infinite_past()};

    std::optional<zx_status_t> dai_format_error_;
    bool codec_start_failed_ = false;
    bool codec_stop_failed_ = false;
  };

  static uint8_t ExpectFormatMatch(const std::shared_ptr<Device>& device,
                                   fuchsia_audio::SampleType sample_type, uint32_t channel_count,
                                   uint32_t rate) {
    std::stringstream stream;
    stream << "Expected format match: [" << sample_type << " " << channel_count << "-channel "
           << rate << " hz]";
    SCOPED_TRACE(stream.str());
    const auto& match = device->SupportedDriverFormatForClientFormat({{
        .sample_type = sample_type,
        .channel_count = channel_count,
        .frames_per_second = rate,
    }});
    EXPECT_TRUE(match);
    return match->pcm_format()->valid_bits_per_sample();
  }

  static void ExpectNoFormatMatch(const std::shared_ptr<Device>& device,
                                  fuchsia_audio::SampleType sample_type, uint32_t channel_count,
                                  uint32_t rate) {
    std::stringstream stream;
    stream << "Unexpected format match: [" << sample_type << " " << channel_count << "-channel "
           << rate << " hz]";
    SCOPED_TRACE(stream.str());
    const auto& match = device->SupportedDriverFormatForClientFormat({{
        .sample_type = sample_type,
        .channel_count = channel_count,
        .frames_per_second = rate,
    }});
    EXPECT_FALSE(match);
  }

  // A consolidated notify recipient for tests (ObserverNotify and ControlNotify).
  std::shared_ptr<NotifyStub> notify() { return notify_; }
  std::shared_ptr<FakeDevicePresenceWatcher> device_presence_watcher() {
    return fake_device_presence_watcher_;
  }

  bool AddObserver(const std::shared_ptr<Device>& device) { return notify()->AddObserver(device); }
  bool SetControl(const std::shared_ptr<Device>& device) { return notify()->SetControl(device); }
  bool DropControl(const std::shared_ptr<Device>& device) { return notify()->DropControl(device); }

  static bool device_plugged_state(const std::shared_ptr<Device>& device) {
    return *device->plug_state_->plugged();
  }

 private:
  static constexpr zx::duration kCommandTimeout = zx::sec(0);

  std::shared_ptr<NotifyStub> notify_;

  // Receives "OnInitCompletion", "DeviceHasError", "DeviceIsRemoved" notifications from Devices.
  std::shared_ptr<FakeDevicePresenceWatcher> fake_device_presence_watcher_;
};

class CodecTest : public DeviceTestBase {
 protected:
  std::unique_ptr<FakeCodec> MakeFakeCodecInput() { return MakeFakeCodec(true); }
  std::unique_ptr<FakeCodec> MakeFakeCodecOutput() { return MakeFakeCodec(false); }
  std::unique_ptr<FakeCodec> MakeFakeCodecNoDirection() { return MakeFakeCodec(std::nullopt); }

  std::shared_ptr<Device> InitializeDeviceForFakeCodec(const std::unique_ptr<FakeCodec>& driver) {
    auto codec_client_end = driver->Enable();
    EXPECT_TRUE(codec_client_end.is_valid());
    auto device =
        Device::Create(std::weak_ptr<FakeDevicePresenceWatcher>(device_presence_watcher()),
                       dispatcher(), "Device name", fuchsia_audio_device::DeviceType::kCodec,
                       fuchsia_audio_device::DriverClient::WithCodec(std::move(codec_client_end)));

    RunLoopUntilIdle();
    EXPECT_FALSE(device->state_ == Device::State::DeviceInitializing) << "Device is initializing";

    return device;
  }

 private:
  std::unique_ptr<FakeCodec> MakeFakeCodec(std::optional<bool> is_input = false) {
    auto codec_endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::Codec>();
    EXPECT_TRUE(codec_endpoints.is_ok());
    auto fake_codec = std::make_unique<FakeCodec>(
        codec_endpoints->server.TakeChannel(), codec_endpoints->client.TakeChannel(), dispatcher());
    fake_codec->set_is_input(is_input);
    return fake_codec;
  }
};

class StreamConfigTest : public DeviceTestBase {
 protected:
  static inline const fuchsia_hardware_audio::Format kDefaultRingBufferFormat{{
      .pcm_format = fuchsia_hardware_audio::PcmFormat{{
          .number_of_channels = 2,
          .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned,
          .bytes_per_sample = 2,
          .valid_bits_per_sample = static_cast<uint8_t>(16),
          .frame_rate = 48000,
      }},
  }};

  std::unique_ptr<FakeStreamConfig> MakeFakeStreamConfigInput() {
    return MakeFakeStreamConfig(true);
  }
  std::unique_ptr<FakeStreamConfig> MakeFakeStreamConfigOutput() {
    return MakeFakeStreamConfig(false);
  }

  std::shared_ptr<Device> InitializeDeviceForFakeStreamConfig(
      const std::unique_ptr<FakeStreamConfig>& driver) {
    auto device_type = *driver->is_input() ? fuchsia_audio_device::DeviceType::kInput
                                           : fuchsia_audio_device::DeviceType::kOutput;
    auto stream_config_client_end = driver->Enable();
    auto device = Device::Create(
        std::weak_ptr<FakeDevicePresenceWatcher>(device_presence_watcher()), dispatcher(),
        "Device name", device_type,
        fuchsia_audio_device::DriverClient::WithStreamConfig(std::move(stream_config_client_end)));

    RunLoopUntilIdle();
    EXPECT_FALSE(device->state_ == Device::State::DeviceInitializing);

    return device;
  }

  static fuchsia_hardware_audio::GainState device_gain_state(
      const std::shared_ptr<Device>& device) {
    return *device->gain_state_;
  }

  static bool SetDeviceGain(const std::shared_ptr<Device>& device,
                            fuchsia_hardware_audio::GainState new_state) {
    return device->SetGain(new_state);
  }

  void ConnectToRingBufferAndExpectValidClient(const std::shared_ptr<Device>& device) {
    EXPECT_TRUE(device->ConnectRingBufferFidl({{
        fuchsia_hardware_audio::PcmFormat{{
            .number_of_channels = 2,
            .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned,
            .bytes_per_sample = 2,
            .valid_bits_per_sample = static_cast<uint8_t>(16),
            .frame_rate = 48000,
        }},
    }}));
    RunLoopUntilIdle();
    EXPECT_TRUE(device->ring_buffer_client_.has_value() && device->ring_buffer_client_->is_valid());
  }

  void GetRingBufferProperties(const std::shared_ptr<Device>& device) {
    ASSERT_TRUE(device->state_ == Device::State::CreatingRingBuffer ||
                device->state_ == Device::State::RingBufferStopped ||
                device->state_ == Device::State::RingBufferStarted);

    device->RetrieveRingBufferProperties();
    RunLoopUntilIdle();
    ASSERT_TRUE(device->ring_buffer_properties_);
  }

  void RetrieveDelayInfoAndExpect(const std::shared_ptr<Device>& device,
                                  std::optional<int64_t> internal_delay,
                                  std::optional<int64_t> external_delay) {
    ASSERT_TRUE(device->state_ == Device::State::CreatingRingBuffer ||
                device->state_ == Device::State::RingBufferStopped ||
                device->state_ == Device::State::RingBufferStarted);
    device->RetrieveDelayInfo();
    RunLoopUntilIdle();

    ASSERT_TRUE(device->delay_info_);
    EXPECT_EQ(device->delay_info_->internal_delay().value_or(0), internal_delay.value_or(0));
    EXPECT_EQ(device->delay_info_->external_delay().value_or(0), external_delay.value_or(0));
  }

  // Accessor for a Device private member.
  static const std::optional<fuchsia_hardware_audio::DelayInfo>& DeviceDelayInfo(
      const std::shared_ptr<Device>& device) {
    return device->delay_info_;
  }

  void GetDriverVmoAndExpectValid(const std::shared_ptr<Device>& device) {
    ASSERT_TRUE(device->state_ == Device::State::CreatingRingBuffer ||
                device->state_ == Device::State::RingBufferStopped ||
                device->state_ == Device::State::RingBufferStarted);

    device->GetVmo(2000, 0);
    RunLoopUntilIdle();
    EXPECT_TRUE(device->VmoReceived());
    EXPECT_TRUE(device->state_ == Device::State::CreatingRingBuffer ||
                device->state_ == Device::State::RingBufferStopped);
  }

  void SetActiveChannelsAndExpect(const std::shared_ptr<Device>& device,
                                  uint64_t expected_bitmask) {
    ASSERT_TRUE(device->state_ == Device::State::CreatingRingBuffer ||
                device->state_ == Device::State::RingBufferStopped ||
                device->state_ == Device::State::RingBufferStarted);

    const auto now = zx::clock::get_monotonic();
    device->SetActiveChannels(expected_bitmask, [device](zx::result<zx::time> result) {
      EXPECT_TRUE(result.is_ok()) << result.status_string();
      ASSERT_TRUE(device->active_channels_set_time_);
      EXPECT_EQ(result.value(), *device->active_channels_set_time_);
    });
    RunLoopUntilIdle();
    ASSERT_TRUE(device->active_channels_set_time_);
    EXPECT_GT(*device->active_channels_set_time_, now);

    ExpectActiveChannels(device, expected_bitmask);
  }

  static void ExpectActiveChannels(const std::shared_ptr<Device>& device,
                                   uint64_t expected_bitmask) {
    EXPECT_EQ(device->active_channels_bitmask_, expected_bitmask);
  }

  void ExpectRingBufferReady(const std::shared_ptr<Device>& device) {
    RunLoopUntilIdle();
    EXPECT_TRUE(device->state_ == Device::State::RingBufferStopped);
  }

  void StartAndExpectValid(const std::shared_ptr<Device>& device) {
    ASSERT_TRUE(device->state_ == Device::State::RingBufferStopped);

    const auto now = zx::clock::get_monotonic().get();
    device->StartRingBuffer([device](zx::result<zx::time> result) {
      EXPECT_TRUE(result.is_ok()) << result.status_string();
      ASSERT_TRUE(device->start_time_);
      EXPECT_EQ(result.value(), *device->start_time_);
    });
    RunLoopUntilIdle();
    ASSERT_TRUE(device->start_time_);
    EXPECT_GT(device->start_time_->get(), now);
    EXPECT_TRUE(device->state_ == Device::State::RingBufferStarted);
  }

  void StopAndExpectValid(const std::shared_ptr<Device>& device) {
    ASSERT_TRUE(device->state_ == Device::State::RingBufferStarted);

    device->StopRingBuffer([](zx_status_t result) { EXPECT_EQ(result, ZX_OK); });
    RunLoopUntilIdle();
    ASSERT_FALSE(device->start_time_);
    EXPECT_TRUE(device->state_ == Device::State::RingBufferStopped);
  }

  static std::shared_ptr<Clock> device_clock(const std::shared_ptr<Device>& device) {
    return device->device_clock_;
  }

 private:
  std::unique_ptr<FakeStreamConfig> MakeFakeStreamConfig(bool is_input = false) {
    auto stream_config_endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::StreamConfig>();
    EXPECT_TRUE(stream_config_endpoints.is_ok());
    auto fake_stream = std::make_unique<FakeStreamConfig>(
        stream_config_endpoints->server.TakeChannel(),
        stream_config_endpoints->client.TakeChannel(), dispatcher());
    fake_stream->set_is_input(is_input);
    return fake_stream;
  }
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_UNITTEST_H_
