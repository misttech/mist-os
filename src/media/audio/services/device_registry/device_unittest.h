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
#include "src/media/audio/services/device_registry/observer_notify.h"
#include "src/media/audio/services/device_registry/testing/fake_codec.h"
#include "src/media/audio/services/device_registry/testing/fake_device_presence_watcher.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"
#include "src/media/audio/services/device_registry/validate.h"

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
  std::unique_ptr<FakeStreamConfig> MakeFakeStreamConfigInput() {
    return MakeFakeStreamConfig(true);
  }
  std::unique_ptr<FakeStreamConfig> MakeFakeStreamConfigOutput() {
    return MakeFakeStreamConfig(false);
  }

  std::unique_ptr<FakeCodec> MakeFakeCodecInput() { return MakeFakeCodec(true); }
  std::unique_ptr<FakeCodec> MakeFakeCodecOutput() { return MakeFakeCodec(false); }
  std::unique_ptr<FakeCodec> MakeFakeCodecNoDirection() { return MakeFakeCodec(std::nullopt); }

  std::shared_ptr<Device> InitializeDeviceForFakeStreamConfig(
      const std::unique_ptr<FakeStreamConfig>& driver) {
    auto device_type = *driver->is_input() ? fuchsia_audio_device::DeviceType::kInput
                                           : fuchsia_audio_device::DeviceType::kOutput;
    auto stream_config_client_end = driver->Enable();
    auto device =
        Device::Create(std::weak_ptr<FakeDevicePresenceWatcher>(fake_device_presence_watcher_),
                       dispatcher(), "Device name", device_type,
                       fuchsia_audio_device::DriverClient::WithStreamConfig(
                           fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(
                               std::move(stream_config_client_end))));

    RunLoopUntilIdle();
    EXPECT_FALSE(device->state_ == Device::State::DeviceInitializing);

    return device;
  }

  std::shared_ptr<Device> InitializeDeviceForFakeCodec(const std::unique_ptr<FakeCodec>& driver) {
    auto codec_client_end = driver->Enable();
    EXPECT_TRUE(codec_client_end.is_valid());
    auto device = Device::Create(
        std::weak_ptr<FakeDevicePresenceWatcher>(fake_device_presence_watcher_), dispatcher(),
        "Device name", fuchsia_audio_device::DeviceType::kCodec,
        fuchsia_audio_device::DriverClient::WithCodec(
            fidl::ClientEnd<fuchsia_hardware_audio::Codec>(std::move(codec_client_end))));

    RunLoopUntilIdle();
    EXPECT_FALSE(device->state_ == Device::State::DeviceInitializing) << "Device is initializing";

    return device;
  }

  static fuchsia_audio_device::Info GetDeviceInfo(std::shared_ptr<Device> device) {
    return *device->info();
  }

  static bool HasError(std::shared_ptr<Device> device) {
    return device->state_ == Device::State::Error;
  }
  static bool InInitializedState(std::shared_ptr<Device> device) {
    return device->state_ == Device::State::DeviceInitialized;
  }
  static bool IsControlled(std::shared_ptr<Device> device) {
    return (device->GetControlNotify() != nullptr);
  }

  bool DevicePluggedState(std::shared_ptr<Device> device) {
    return *device->plug_state_->plugged();
  }
  fuchsia_hardware_audio::GainState DeviceGainState(std::shared_ptr<Device> device) {
    return *device->gain_state_;
  }
  class NotifyStub : public std::enable_shared_from_this<NotifyStub>, public ControlNotify {
   public:
    explicit NotifyStub(DeviceTestBase& parent) : parent_(parent) {}
    virtual ~NotifyStub() = default;

    bool AddObserver(std::shared_ptr<Device> device) {
      return device->AddObserver(shared_from_this());
    }
    bool SetControl(std::shared_ptr<Device> device) {
      return device->SetControl(shared_from_this());
    }
    bool DropControl(std::shared_ptr<Device> device) { return device->DropControl(); }

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

      if (!dai_format.has_value()) {
        FX_LOGS(INFO) << "DaiFormat was cleared";
        dai_format_.reset();
        codec_format_info_.reset();
      } else {
        FX_LOGS(INFO) << __func__;
        LogDaiFormat(dai_format);
        LogCodecFormatInfo(codec_format_info);
        FX_DCHECK(codec_format_info.has_value());
        dai_format_ = *dai_format;
        codec_format_info_ = codec_format_info;
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

  static uint8_t ExpectFormatMatch(std::shared_ptr<Device> device,
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

  static void ExpectNoFormatMatch(std::shared_ptr<Device> device,
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

  // A consolidated notify recipient for tests (ObserverNotify, and in future CL, ControlNotify).
  std::shared_ptr<NotifyStub> notify() { return notify_; }

  static bool SetDeviceGain(std::shared_ptr<Device> device,
                            fuchsia_hardware_audio::GainState new_state) {
    return device->SetGain(new_state);
  }

  bool AddObserver(std::shared_ptr<Device> device) { return notify()->AddObserver(device); }
  bool SetControl(std::shared_ptr<Device> device) { return notify()->SetControl(device); }
  bool DropControl(std::shared_ptr<Device> device) { return notify()->DropControl(device); }

  void ConnectToRingBufferAndExpectValidClient(std::shared_ptr<Device> device) {
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
    EXPECT_TRUE(device->ring_buffer_client_.is_valid());
  }

  void GetRingBufferProperties(std::shared_ptr<Device> device) {
    ASSERT_TRUE(device->state_ == Device::State::CreatingRingBuffer ||
                device->state_ == Device::State::RingBufferStopped ||
                device->state_ == Device::State::RingBufferStarted);

    device->RetrieveRingBufferProperties();
    RunLoopUntilIdle();
    ASSERT_TRUE(device->ring_buffer_properties_);
  }

  void RetrieveDelayInfoAndExpect(std::shared_ptr<Device> device,
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

  void GetDriverVmoAndExpectValid(std::shared_ptr<Device> device) {
    ASSERT_TRUE(device->state_ == Device::State::CreatingRingBuffer ||
                device->state_ == Device::State::RingBufferStopped ||
                device->state_ == Device::State::RingBufferStarted);

    device->GetVmo(2000, 0);
    RunLoopUntilIdle();
    EXPECT_TRUE(device->VmoReceived());
    EXPECT_TRUE(device->state_ == Device::State::CreatingRingBuffer ||
                device->state_ == Device::State::RingBufferStopped);
  }

  void SetActiveChannelsAndExpect(std::shared_ptr<Device> device, uint64_t expected_bitmask) {
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

  static void ExpectActiveChannels(std::shared_ptr<Device> device, uint64_t expected_bitmask) {
    EXPECT_EQ(device->active_channels_bitmask_, expected_bitmask);
  }

  void ExpectRingBufferReady(std::shared_ptr<Device> device) {
    RunLoopUntilIdle();
    EXPECT_TRUE(device->state_ == Device::State::RingBufferStopped);
  }

  void StartAndExpectValid(std::shared_ptr<Device> device) {
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

  void StopAndExpectValid(std::shared_ptr<Device> device) {
    ASSERT_TRUE(device->state_ == Device::State::RingBufferStarted);

    device->StopRingBuffer([](zx_status_t result) { EXPECT_EQ(result, ZX_OK); });
    RunLoopUntilIdle();
    ASSERT_FALSE(device->start_time_);
    EXPECT_TRUE(device->state_ == Device::State::RingBufferStopped);
  }

  static std::shared_ptr<Clock> device_clock(std::shared_ptr<Device> device) {
    return device->device_clock_;
  }

  ////////////////////////
  // Codec-related methods
  static fuchsia_hardware_audio::DaiFormat SafeDaiFormatFromDaiSupportedFormats(
      const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets) {
    FX_CHECK(!dai_format_sets.empty()) << "empty DaiSupportedFormats";

    FX_CHECK(!dai_format_sets[0].number_of_channels().empty() &&
             !dai_format_sets[0].sample_formats().empty() &&
             !dai_format_sets[0].frame_formats().empty() &&
             !dai_format_sets[0].frame_rates().empty() &&
             !dai_format_sets[0].bits_per_slot().empty() &&
             !dai_format_sets[0].bits_per_sample().empty())
        << "empty sub-vector in DaiSupportedFormats";

    fuchsia_hardware_audio::DaiFormat dai_format{{
        .number_of_channels = dai_format_sets[0].number_of_channels()[0],
        .channels_to_use_bitmask =
            (dai_format_sets[0].number_of_channels()[0] < 64
                 ? (1ull << dai_format_sets[0].number_of_channels()[0]) - 1ull
                 : 0xFFFFFFFFFFFFFFFFull),
        .sample_format = dai_format_sets[0].sample_formats()[0],
        .frame_format = dai_format_sets[0].frame_formats()[0],
        .frame_rate = dai_format_sets[0].frame_rates()[0],
        .bits_per_slot = dai_format_sets[0].bits_per_slot()[0],
        .bits_per_sample = dai_format_sets[0].bits_per_sample()[0],
    }};
    FX_CHECK(ValidateDaiFormat(dai_format) == ZX_OK)
        << "first entries did not create a valid DaiFormat";

    return dai_format;
  }

  static fuchsia_hardware_audio::DaiFormat SecondDaiFormatFromDaiSupportedFormats(
      const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets) {
    auto safe_format_2 = SafeDaiFormatFromDaiSupportedFormats(dai_format_sets);

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
      return fuchsia_hardware_audio::DaiFormat{{
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
      FX_CHECK(false) << "Dai format set has only one possible valid format";
    }
    return safe_format_2;
  }

  static fuchsia_hardware_audio::DaiFormat UnsupportedDaiFormatFromDaiFormatSets(
      const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& dai_format_sets) {
    FX_CHECK(!dai_format_sets.empty()) << "empty DaiSupportedFormats";

    std::optional<fuchsia_hardware_audio::DaiFormat> dai_format;
    for (const auto& format_set : dai_format_sets) {
      FX_CHECK(!format_set.number_of_channels().empty() && !format_set.sample_formats().empty() &&
               !format_set.frame_formats().empty() && !format_set.frame_rates().empty() &&
               !format_set.bits_per_slot().empty() && !format_set.bits_per_sample().empty())
          << "empty sub-vector in DaiSupportedFormats";
      dai_format = SafeDaiFormatFromDaiSupportedFormats({{format_set}});
      if (dai_format->number_of_channels() > 1) {
        dai_format->number_of_channels() -= 1;
        dai_format->channels_to_use_bitmask() = (1ull << dai_format->number_of_channels()) - 1ull;
        FX_LOGS(INFO) << "Returning this invalid format: ";
        LogDaiFormat(dai_format);
        return *dai_format;
      }
      if (dai_format->frame_rate() > kMinSupportedDaiFrameRate) {
        dai_format->frame_rate() -= 1;
        FX_LOGS(INFO) << "Returning this invalid format: ";
        LogDaiFormat(dai_format);
        return *dai_format;
      }
      if (dai_format->bits_per_slot() > 1) {
        dai_format->bits_per_slot() -= 1;
        FX_LOGS(INFO) << "Returning this invalid format: ";
        LogDaiFormat(dai_format);
        return *dai_format;
      }
      if (dai_format->bits_per_sample() > 1) {
        dai_format->bits_per_sample() -= 1;
        FX_LOGS(INFO) << "Returning this invalid format: ";
        LogDaiFormat(dai_format);
        return *dai_format;
      }
    }

    FX_CHECK(false) << "No invalid DaiFormat found for these format_sets";
    __UNREACHABLE;
  }

  /////////////////////////////
  // General consts and members
  static constexpr zx::duration kCommandTimeout = zx::sec(0);

  std::shared_ptr<NotifyStub> notify_;

  // Receives "OnInitCompletion", "DeviceHasError", "DeviceIsRemoved" notifications from Devices.
  std::shared_ptr<FakeDevicePresenceWatcher> fake_device_presence_watcher_;

 private:
  std::unique_ptr<FakeStreamConfig> MakeFakeStreamConfig(bool is_input = false) {
    zx::channel server_end, client_end;
    EXPECT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
    auto fake_stream = std::make_unique<FakeStreamConfig>(std::move(server_end),
                                                          std::move(client_end), dispatcher());
    fake_stream->set_is_input(is_input);
    return fake_stream;
  }

  std::unique_ptr<FakeCodec> MakeFakeCodec(std::optional<bool> is_input = false) {
    zx::channel server_end, client_end;
    EXPECT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
    auto fake_codec =
        std::make_unique<FakeCodec>(std::move(server_end), std::move(client_end), dispatcher());
    fake_codec->set_is_input(is_input);
    return fake_codec;
  }
};

}  // namespace media_audio

#endif  // SRC_MEDIA_AUDIO_SERVICES_DEVICE_REGISTRY_DEVICE_UNITTEST_H_
