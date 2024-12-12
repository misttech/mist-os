// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_MEDIA_RVICES_DEVICE_REGISTRY_DEVICE_UNITTEST_H_
#define SRC_MEDIA_RVICES_DEVICE_REGISTRY_DEVICE_UNITTEST_H_

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <memory>
#include <optional>
#include <sstream>
#include <unordered_map>

#include <gtest/gtest.h>

#include "src/lib/testing/loop_fixture/test_loop_fixture.h"
#include "src/media/audio/lib/clock/clock.h"
#include "src/media/audio/services/device_registry/basic_types.h"
#include "src/media/audio/services/device_registry/control_notify.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/inspector.h"
#include "src/media/audio/services/device_registry/logging.h"
#include "src/media/audio/services/device_registry/testing/fake_codec.h"
#include "src/media/audio/services/device_registry/testing/fake_composite.h"
#include "src/media/audio/services/device_registry/testing/fake_device_presence_watcher.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"

namespace media_audio {

static constexpr bool kLogDeviceTestNotifyResponses = false;

// Test class to verify the driver initialization/configuration sequence.
class DeviceTestBase : public gtest::TestLoopFixture {
  static inline const std::string_view kClassName = "DeviceTestBase";

 public:
  void SetUp() override {
    // Use our production Inspector during device unittests.
    media_audio::Inspector::Initialize(dispatcher());

    notify_ = std::make_shared<NotifyStub>(*this);
    fake_device_presence_watcher_ = std::make_shared<FakeDevicePresenceWatcher>();
  }
  void TearDown() override { fake_device_presence_watcher_.reset(); }

 protected:
  static fuchsia_audio_device::Info GetDeviceInfo(const std::shared_ptr<Device>& device) {
    return *device->info();
  }

  static std::shared_ptr<Clock> device_clock(const std::shared_ptr<Device>& device) {
    return device->device_clock_;
  }

  static bool IsControlled(const std::shared_ptr<Device>& device) {
    return (device->GetControlNotify() != nullptr);
  }

  static bool HasRingBuffer(const std::shared_ptr<Device>& device, ElementId element_id) {
    return device->ring_buffer_map_.find(element_id) != device->ring_buffer_map_.end();
  }

  static bool RingBufferIsCreatingOrStopped(const std::shared_ptr<Device>& device,
                                            ElementId element_id) {
    auto match = device->ring_buffer_map_.find(element_id);

    return (HasRingBuffer(device, element_id) &&
            (match->second.ring_buffer_state == Device::RingBufferState::Creating ||
             match->second.ring_buffer_state == Device::RingBufferState::Stopped));
  }
  static bool RingBufferIsOperational(const std::shared_ptr<Device>& device, ElementId element_id) {
    auto match = device->ring_buffer_map_.find(element_id);

    return (HasRingBuffer(device, element_id) &&
            (match->second.ring_buffer_state == Device::RingBufferState::Stopped ||
             match->second.ring_buffer_state == Device::RingBufferState::Started));
  }
  static bool RingBufferIsStopped(const std::shared_ptr<Device>& device, ElementId element_id) {
    auto match = device->ring_buffer_map_.find(element_id);

    return (HasRingBuffer(device, element_id) &&
            match->second.ring_buffer_state == Device::RingBufferState::Stopped);
  }
  static bool RingBufferIsStarted(const std::shared_ptr<Device>& device, ElementId element_id) {
    auto match = device->ring_buffer_map_.find(element_id);

    return (HasRingBuffer(device, element_id) &&
            match->second.ring_buffer_state == Device::RingBufferState::Started);
  }
  static void GetDaiFormatSets(
      const std::shared_ptr<Device>& device, ElementId element_id,
      fit::callback<void(ElementId,
                         const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>&)>
          dai_format_sets_callback) {
    device->GetDaiFormatSets(element_id, std::move(dai_format_sets_callback));
  }

  // Accessor for a Device private member.
  static const std::optional<fuchsia_hardware_audio::DelayInfo>& DeviceDelayInfo(
      const std::shared_ptr<Device>& device, ElementId element_id) {
    return device->ring_buffer_map_.find(element_id)->second.delay_info;
  }

  class NotifyStub : public std::enable_shared_from_this<NotifyStub>, public ControlNotify {
    static inline const std::string_view kClassName = "DeviceTestBase::NotifyStub";

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
    void DeviceIsRemoved() final { ADR_LOG_OBJECT(kLogDeviceTestNotifyResponses); }
    void DeviceHasError() final { ADR_LOG_OBJECT(kLogDeviceTestNotifyResponses); }
    void GainStateIsChanged(const fuchsia_audio_device::GainState& new_gain_state) final {
      ADR_LOG_OBJECT(kLogDeviceTestNotifyResponses);
      gain_state_ = new_gain_state;
    }
    void PlugStateIsChanged(const fuchsia_audio_device::PlugState& new_plug_state,
                            zx::time plug_change_time) final {
      ADR_LOG_OBJECT(kLogDeviceTestNotifyResponses);
      plug_state_ = std::make_pair(new_plug_state, plug_change_time);
    }
    void TopologyIsChanged(TopologyId topology_id) final {
      ADR_LOG_OBJECT(kLogDeviceTestNotifyResponses) << "(topology_id " << topology_id << ")";
      topology_id_ = topology_id;
    }
    void ElementStateIsChanged(
        ElementId element_id,
        fuchsia_hardware_audio_signalprocessing::ElementState element_state) final {
      ADR_LOG_OBJECT(kLogDeviceTestNotifyResponses) << "(element_id " << element_id << ")";
      element_states_.insert({element_id, element_state});
    }

    // ControlNotify
    //
    void DeviceDroppedRingBuffer(ElementId element_id) final {
      ADR_LOG_OBJECT(kLogDeviceTestNotifyResponses) << "(element_id " << element_id << ")";
    }
    void DelayInfoIsChanged(ElementId element_id,
                            const fuchsia_audio_device::DelayInfo& new_delay_info) final {
      ADR_LOG_OBJECT(kLogDeviceTestNotifyResponses) << "(element_id " << element_id << ")";
      delay_infos_.insert_or_assign(element_id, new_delay_info);
    }
    void DaiFormatIsChanged(
        ElementId element_id, const std::optional<fuchsia_hardware_audio::DaiFormat>& dai_format,
        const std::optional<fuchsia_hardware_audio::CodecFormatInfo>& codec_format_info) final {
      ADR_LOG_OBJECT(kLogDeviceTestNotifyResponses) << "(element_id " << element_id << ")";
      dai_format_errors_.erase(element_id);

      codec_format_infos_.erase(element_id);
      if (dai_format.has_value()) {
        LogDaiFormat(dai_format);
        LogCodecFormatInfo(codec_format_info);
        dai_formats_.insert_or_assign(element_id, *dai_format);
        if (codec_format_info.has_value()) {
          codec_format_infos_.insert({element_id, *codec_format_info});
        }
      } else {
        dai_formats_.insert_or_assign(element_id, std::nullopt);
      }
    }
    void DaiFormatIsNotChanged(ElementId element_id,
                               const fuchsia_hardware_audio::DaiFormat& dai_format,
                               fuchsia_audio_device::ControlSetDaiFormatError error) final {
      ADR_LOG_OBJECT(kLogDeviceTestNotifyResponses)
          << "(element_id " << element_id << ", " << error << ")";
      dai_format_errors_.insert_or_assign(element_id, error);
    }

    void CodecIsStarted(const zx::time& start_time) final {
      ADR_LOG_OBJECT(kLogDeviceTestNotifyResponses) << "(" << start_time.get() << ")";
      codec_start_failed_ = false;
      codec_start_time_ = start_time;
      codec_stop_time_.reset();
    }
    void CodecIsNotStarted() final {
      ADR_LOG_OBJECT(kLogDeviceTestNotifyResponses);
      codec_start_failed_ = true;
    }
    void CodecIsStopped(const zx::time& stop_time) final {
      ADR_LOG_OBJECT(kLogDeviceTestNotifyResponses) << "(" << stop_time.get() << ")";
      codec_stop_failed_ = false;
      codec_stop_time_ = stop_time;
      codec_start_time_.reset();
    }
    void CodecIsNotStopped() final {
      ADR_LOG_OBJECT(kLogDeviceTestNotifyResponses);
      codec_stop_failed_ = true;
    }
    void DeviceIsReset() final {
      ADR_LOG_OBJECT(kLogDeviceTestNotifyResponses);
      device_is_reset_ = true;
    }

    // control and access internal state, for validating that correct responses were received.
    //
    // For testing purposes, reset internal state so we detect new Notify calls (including errors).
    void clear_dai_formats() {
      dai_formats_.clear();
      dai_format_errors_.clear();
      codec_format_infos_.clear();
    }
    void clear_dai_format(ElementId element_id) {
      dai_formats_.erase(element_id);
      dai_format_errors_.erase(element_id);
      codec_format_infos_.erase(element_id);
    }
    // If Codec/Start and Stop is added to Composite, then move these into a map like DaiFormat is.
    void clear_codec_start_stop() {
      codec_start_time_.reset();
      codec_stop_time_ = zx::time::infinite_past();
      codec_start_failed_ = false;
      codec_stop_failed_ = false;
    }
    bool codec_is_started() {
      FX_CHECK(codec_start_time_.has_value() != codec_stop_time_.has_value());
      return codec_start_time_.has_value();
    }
    bool codec_is_stopped() {
      FX_CHECK(codec_start_time_.has_value() != codec_stop_time_.has_value());
      return codec_stop_time_.has_value();
    }

    const std::optional<fuchsia_audio_device::GainState>& gain_state() const { return gain_state_; }
    std::optional<fuchsia_audio_device::GainState>& gain_state() { return gain_state_; }

    const std::optional<std::pair<fuchsia_audio_device::PlugState, zx::time>>& plug_state() const {
      return plug_state_;
    }
    std::optional<std::pair<fuchsia_audio_device::PlugState, zx::time>>& plug_state() {
      return plug_state_;
    }

    std::optional<fuchsia_audio_device::DelayInfo> delay_info(
        ElementId element_id = fuchsia_audio_device::kDefaultRingBufferElementId) const {
      auto delay_match = delay_infos_.find(element_id);
      if (delay_match == delay_infos_.end()) {
        return std::nullopt;
      }
      return delay_match->second;
    }
    void clear_delay_info(
        ElementId element_id = fuchsia_audio_device::kDefaultRingBufferElementId) {
      delay_infos_.erase(element_id);
    }
    void clear_delay_infos() { delay_infos_.clear(); }

    std::optional<fuchsia_hardware_audio::DaiFormat> dai_format(
        ElementId element_id = fuchsia_audio_device::kDefaultDaiInterconnectElementId) {
      if (dai_formats_.find(element_id) == dai_formats_.end()) {
        return std::nullopt;
      }
      return dai_formats_.at(element_id);
    }
    std::optional<fuchsia_hardware_audio::CodecFormatInfo> codec_format_info(ElementId element_id) {
      if (codec_format_infos_.find(element_id) == codec_format_infos_.end()) {
        return std::nullopt;
      }
      return codec_format_infos_.at(element_id);
    }
    const std::unordered_map<ElementId, std::optional<fuchsia_hardware_audio::DaiFormat>>&
    dai_formats() {
      return dai_formats_;
    }
    const std::unordered_map<ElementId, fuchsia_hardware_audio::CodecFormatInfo>&
    codec_format_infos() {
      return codec_format_infos_;
    }
    const std::unordered_map<ElementId, fuchsia_audio_device::ControlSetDaiFormatError>&
    dai_format_errors() {
      return dai_format_errors_;
    }

    std::optional<zx::time>& codec_start_time() { return codec_start_time_; }
    bool codec_start_failed() const { return codec_start_failed_; }
    std::optional<zx::time>& codec_stop_time() { return codec_stop_time_; }
    bool codec_stop_failed() const { return codec_stop_failed_; }
    bool device_is_reset() const { return device_is_reset_; }

    const std::unordered_map<ElementId, fuchsia_hardware_audio_signalprocessing::ElementState>&
    element_states() const {
      return element_states_;
    }
    void clear_element_states() { element_states_.clear(); }

    std::optional<TopologyId> topology_id() const { return topology_id_; }
    void clear_topology_id() { topology_id_.reset(); }

   private:
    [[maybe_unused]] DeviceTestBase& parent_;
    std::optional<fuchsia_audio_device::GainState> gain_state_;
    std::optional<std::pair<fuchsia_audio_device::PlugState, zx::time>> plug_state_;
    std::unordered_map<ElementId, fuchsia_audio_device::DelayInfo> delay_infos_;

    std::unordered_map<ElementId, std::optional<fuchsia_hardware_audio::DaiFormat>> dai_formats_;
    std::unordered_map<ElementId, fuchsia_audio_device::ControlSetDaiFormatError>
        dai_format_errors_;
    std::unordered_map<ElementId, fuchsia_hardware_audio::CodecFormatInfo> codec_format_infos_;

    std::optional<zx::time> codec_start_time_;
    std::optional<zx::time> codec_stop_time_{zx::time::infinite_past()};
    bool codec_start_failed_ = false;
    bool codec_stop_failed_ = false;
    bool device_is_reset_ = false;

    std::optional<TopologyId> topology_id_;
    std::unordered_map<ElementId, fuchsia_hardware_audio_signalprocessing::ElementState>
        element_states_;
  };

  static uint8_t ExpectFormatMatch(const std::shared_ptr<Device>& device, ElementId element_id,
                                   fuchsia_audio::SampleType sample_type, uint32_t channel_count,
                                   uint32_t rate) {
    std::stringstream stream;
    stream << "Expected format match: [" << sample_type << " " << channel_count << "-channel "
           << rate << " hz]";
    SCOPED_TRACE(stream.str());
    const auto& match =
        device->SupportedDriverFormatForClientFormat(element_id, {{
                                                                     .sample_type = sample_type,
                                                                     .channel_count = channel_count,
                                                                     .frames_per_second = rate,
                                                                 }});
    EXPECT_TRUE(match);
    return match->pcm_format()->valid_bits_per_sample();
  }

  static void ExpectNoFormatMatch(const std::shared_ptr<Device>& device, ElementId element_id,
                                  fuchsia_audio::SampleType sample_type, uint32_t channel_count,
                                  uint32_t rate) {
    std::stringstream stream;
    stream << "Unexpected format match: [" << sample_type << " " << channel_count << "-channel "
           << rate << " hz]";
    SCOPED_TRACE(stream.str());
    const auto& match =
        device->SupportedDriverFormatForClientFormat(element_id, {{
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
  static bool DropControl(const std::shared_ptr<Device>& device) {
    return NotifyStub::DropControl(device);
  }

  static bool device_plugged_state(const std::shared_ptr<Device>& device) {
    return *device->plug_state_->plugged();
  }

  static ElementId ring_buffer_id() { return kRingBufferElementId; }
  static ElementId dai_id() { return kDaiElementId; }

 private:
  static constexpr ElementId kRingBufferElementId =
      fuchsia_audio_device::kDefaultRingBufferElementId;
  static constexpr ElementId kDaiElementId = fuchsia_audio_device::kDefaultDaiInterconnectElementId;

  static constexpr zx::duration kCommandTimeout = zx::sec(0);

  std::shared_ptr<NotifyStub> notify_;

  // Receives "OnInitCompletion", "DeviceHasError", "DeviceIsRemoved" notifications from Devices.
  std::shared_ptr<FakeDevicePresenceWatcher> fake_device_presence_watcher_;
};

class CodecTest : public DeviceTestBase {
 protected:
  std::shared_ptr<FakeCodec> MakeFakeCodecInput() { return MakeFakeCodec(true); }
  std::shared_ptr<FakeCodec> MakeFakeCodecOutput() { return MakeFakeCodec(false); }
  std::shared_ptr<FakeCodec> MakeFakeCodecNoDirection() { return MakeFakeCodec(std::nullopt); }

  std::shared_ptr<Device> InitializeDeviceForFakeCodec(const std::shared_ptr<FakeCodec>& driver) {
    auto codec_client_end = driver->Enable();
    EXPECT_TRUE(codec_client_end.is_valid());
    auto device =
        Device::Create(std::weak_ptr<FakeDevicePresenceWatcher>(device_presence_watcher()),
                       dispatcher(), "Codec device name", fuchsia_audio_device::DeviceType::kCodec,
                       fuchsia_audio_device::DriverClient::WithCodec(std::move(codec_client_end)));

    RunLoopUntilIdle();
    EXPECT_TRUE(device->is_operational() || device->has_error()) << "device still initializing";

    return device;
  }

 private:
  std::shared_ptr<FakeCodec> MakeFakeCodec(std::optional<bool> is_input = false) {
    auto codec_endpoints = fidl::Endpoints<fuchsia_hardware_audio::Codec>::Create();
    auto fake_codec = std::make_shared<FakeCodec>(
        codec_endpoints.server.TakeChannel(), codec_endpoints.client.TakeChannel(), dispatcher());
    fake_codec->set_is_input(is_input);
    return fake_codec;
  }
};

class CompositeTest : public DeviceTestBase {
 protected:
  static inline const std::string_view kClassName = "CompositeTest";

  static const std::vector<
      std::pair<ElementId, std::vector<fuchsia_hardware_audio::SupportedFormats>>>&
  ElementDriverRingBufferFormatSets(const std::shared_ptr<Device>& device) {
    return device->element_driver_ring_buffer_format_sets_;
  }

  static const std::unordered_map<ElementId, ElementRecord>& signal_processing_elements(
      const std::shared_ptr<Device>& device) {
    return device->sig_proc_element_map_;
  }

  std::shared_ptr<FakeComposite> MakeFakeComposite() {
    auto composite_endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::Composite>();
    EXPECT_TRUE(composite_endpoints.is_ok());
    auto fake_composite =
        std::make_shared<FakeComposite>(composite_endpoints->server.TakeChannel(),
                                        composite_endpoints->client.TakeChannel(), dispatcher());
    return fake_composite;
  }

  std::shared_ptr<Device> InitializeDeviceForFakeComposite(
      const std::shared_ptr<FakeComposite>& driver) {
    auto composite_client_end = driver->Enable();
    EXPECT_TRUE(composite_client_end.is_valid());
    auto device = Device::Create(
        std::weak_ptr<FakeDevicePresenceWatcher>(device_presence_watcher()), dispatcher(),
        "Composite device name", fuchsia_audio_device::DeviceType::kComposite,
        fuchsia_audio_device::DriverClient::WithComposite(std::move(composite_client_end)));

    RunLoopUntilIdle();
    EXPECT_TRUE(device->is_operational() || device->has_error()) << "device still initializing";

    return device;
  }

  void TestCreateRingBuffer(const std::shared_ptr<Device>& device, ElementId element_id,
                            const fuchsia_hardware_audio::Format& safe_format);

  bool ExpectDaiFormatMatches(ElementId dai_id,
                              const fuchsia_hardware_audio::DaiFormat& dai_format) {
    auto format_match = notify()->dai_formats().find(dai_id);
    if (format_match == notify()->dai_formats().end()) {
      ADR_WARN_METHOD() << "Dai element " << dai_id << " not found";
      return false;
    }
    if (!format_match->second.has_value()) {
      ADR_WARN_METHOD() << "Dai format not set for element " << dai_id;
      return false;
    }
    if (*format_match->second != dai_format) {
      ADR_WARN_METHOD() << "Dai format for element " << dai_id << " is not the expected";
      return false;
    }
    return true;
  }

  bool ExpectDaiFormatError(ElementId element_id,
                            fuchsia_audio_device::ControlSetDaiFormatError expected_error) {
    auto error_match = notify()->dai_format_errors().find(element_id);
    if (error_match == notify()->dai_format_errors().end()) {
      ADR_WARN_METHOD() << "No dai format errors for element " << element_id;
      return false;
    }

    if (error_match->second != expected_error) {
      ADR_WARN_METHOD() << "For element " << element_id << ", expected error " << expected_error
                        << " but instead received " << error_match->second;
      return false;
    }
    return true;
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
  static constexpr uint32_t kDefaultRequestedRingBufferBytes = 2000;

  std::shared_ptr<FakeStreamConfig> MakeFakeStreamConfigInput() {
    return MakeFakeStreamConfig(true);
  }
  std::shared_ptr<FakeStreamConfig> MakeFakeStreamConfigOutput() {
    return MakeFakeStreamConfig(false);
  }

  std::shared_ptr<Device> InitializeDeviceForFakeStreamConfig(
      const std::shared_ptr<FakeStreamConfig>& driver) {
    auto device_type = *driver->is_input() ? fuchsia_audio_device::DeviceType::kInput
                                           : fuchsia_audio_device::DeviceType::kOutput;
    auto stream_config_client_end = driver->Enable();
    auto device = Device::Create(
        std::weak_ptr<FakeDevicePresenceWatcher>(device_presence_watcher()), dispatcher(),
        "StreamConfig device name", device_type,
        fuchsia_audio_device::DriverClient::WithStreamConfig(std::move(stream_config_client_end)));

    RunLoopUntilIdle();
    EXPECT_TRUE(device->is_operational() || device->has_error()) << "device still initializing";

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

  void ConnectToRingBufferAndExpectValidClient(const std::shared_ptr<Device>& device,
                                               ElementId element_id) {
    EXPECT_EQ(
        device->ConnectRingBufferFidl(
            element_id, {{
                            fuchsia_hardware_audio::PcmFormat{{
                                .number_of_channels = 2,
                                .sample_format = fuchsia_hardware_audio::SampleFormat::kPcmSigned,
                                .bytes_per_sample = 2,
                                .valid_bits_per_sample = static_cast<uint8_t>(16),
                                .frame_rate = 48000,
                            }},
                        }}),
        fuchsia_audio_device::ControlCreateRingBufferError(0));

    RunLoopUntilIdle();
    EXPECT_TRUE(device->ring_buffer_map_.find(element_id)->second.ring_buffer_client.has_value() &&
                device->ring_buffer_map_.find(element_id)->second.ring_buffer_client->is_valid());
  }

  void GetRingBufferProperties(const std::shared_ptr<Device>& device, ElementId element_id) {
    ASSERT_TRUE(RingBufferIsCreatingOrStopped(device, element_id));

    device->RetrieveRingBufferProperties(element_id);

    RunLoopUntilIdle();
    ASSERT_TRUE(
        device->ring_buffer_map_.find(element_id)->second.ring_buffer_properties.has_value());
    EXPECT_TRUE(RingBufferIsCreatingOrStopped(device, element_id));
  }

  void RetrieveDelayInfoAndExpect(const std::shared_ptr<Device>& device, ElementId element_id,
                                  std::optional<int64_t> internal_delay,
                                  std::optional<int64_t> external_delay) {
    ASSERT_TRUE(RingBufferIsOperational(device, element_id));
    device->RetrieveDelayInfo(element_id);

    RunLoopUntilIdle();
    ASSERT_TRUE(device->ring_buffer_map_.find(element_id)->second.delay_info.has_value());
    EXPECT_EQ(
        device->ring_buffer_map_.find(element_id)->second.delay_info->internal_delay().value_or(0),
        internal_delay.value_or(0));
    EXPECT_EQ(
        device->ring_buffer_map_.find(element_id)->second.delay_info->external_delay().value_or(0),
        external_delay.value_or(0));
  }

  void GetDriverVmoAndExpectValid(const std::shared_ptr<Device>& device, ElementId element_id) {
    ASSERT_TRUE(RingBufferIsCreatingOrStopped(device, element_id));

    device->GetVmo(element_id, 2000, 0);

    RunLoopUntilIdle();
    EXPECT_TRUE(device->VmoReceived(element_id));
    EXPECT_TRUE(RingBufferIsCreatingOrStopped(device, element_id));
  }

  void SetActiveChannelsAndExpect(const std::shared_ptr<Device>& device, ElementId element_id,
                                  uint64_t expected_bitmask) {
    ASSERT_TRUE(RingBufferIsCreatingOrStopped(device, element_id));
    const auto now = zx::clock::get_monotonic();
    auto& ring_buffer = device->ring_buffer_map_.find(element_id)->second;
    bool callback_received = false;

    auto succeeded = device->SetActiveChannels(
        element_id, expected_bitmask,
        [&ring_buffer, &callback_received](zx::result<zx::time> result) {
          EXPECT_TRUE(result.is_ok()) << result.status_string();
          ASSERT_TRUE(ring_buffer.set_active_channels_completed_at.has_value());
          EXPECT_EQ(*result, *ring_buffer.set_active_channels_completed_at);
          callback_received = true;
        });

    RunLoopUntilIdle();
    ASSERT_TRUE(succeeded);
    ASSERT_TRUE(ring_buffer.set_active_channels_completed_at.has_value());
    EXPECT_GT(*ring_buffer.set_active_channels_completed_at, now);
    ExpectActiveChannels(device, element_id, expected_bitmask);
  }

  // This driver supports SetActiveChannels, and a channel config has already been set. This
  // SetActiveChannels() represents no change and should succeed; the set-time should not change.
  void SetActiveChannelsAndExpectNoUpdate(const std::shared_ptr<Device>& device,
                                          ElementId element_id, uint64_t expected_bitmask) {
    ASSERT_TRUE(RingBufferIsCreatingOrStopped(device, element_id));
    auto& ring_buffer = device->ring_buffer_map_.find(element_id)->second;
    ASSERT_TRUE(ring_buffer.set_active_channels_completed_at.has_value())
        << "SetActiveChannels has not yet been called";
    auto previous_set_active_channels_completed_at = *ring_buffer.set_active_channels_completed_at;
    bool callback_received = false;

    auto succeeded = device->SetActiveChannels(
        element_id, expected_bitmask,
        [&ring_buffer, &callback_received](zx::result<zx::time> result) {
          EXPECT_TRUE(result.is_ok()) << result.status_string();
          ASSERT_TRUE(ring_buffer.set_active_channels_completed_at.has_value());
          EXPECT_EQ(*result, *ring_buffer.set_active_channels_completed_at);
          callback_received = true;
        });

    RunLoopUntilIdle();
    ASSERT_TRUE(succeeded);
    ASSERT_TRUE(ring_buffer.set_active_channels_completed_at.has_value());
    EXPECT_EQ(*ring_buffer.set_active_channels_completed_at,
              previous_set_active_channels_completed_at);
    ExpectActiveChannels(device, element_id, expected_bitmask);
  }

  static void ExpectActiveChannels(const std::shared_ptr<Device>& device, ElementId element_id,
                                   uint64_t expected_bitmask) {
    ASSERT_TRUE(device->ring_buffer_map_.find(element_id) != device->ring_buffer_map_.end());
    ASSERT_TRUE(
        device->ring_buffer_map_.find(element_id)->second.active_channels_bitmask.has_value());
    EXPECT_EQ(*device->ring_buffer_map_.find(element_id)->second.active_channels_bitmask,
              expected_bitmask);
  }

  static void ExpectActiveChannelsOrUnset(const std::shared_ptr<Device>& device,
                                          ElementId element_id, uint64_t expected_bitmask) {
    ASSERT_TRUE(device->ring_buffer_map_.find(element_id) != device->ring_buffer_map_.end());
    auto bitmask = device->ring_buffer_map_.find(element_id)->second.active_channels_bitmask;
    EXPECT_TRUE(!bitmask.has_value() || *bitmask == expected_bitmask);
  }

  void ExpectRingBufferReady(const std::shared_ptr<Device>& device, ElementId element_id) {
    RunLoopUntilIdle();
    EXPECT_TRUE(device->is_operational());
    EXPECT_TRUE(RingBufferIsStopped(device, element_id));
  }

  void StartAndExpectValid(const std::shared_ptr<Device>& device, ElementId element_id) {
    ASSERT_TRUE(device->is_operational());
    ASSERT_TRUE(RingBufferIsStopped(device, element_id));
    const auto now = zx::clock::get_monotonic().get();
    auto& ring_buffer = device->ring_buffer_map_.find(element_id)->second;

    device->StartRingBuffer(element_id, [&ring_buffer](zx::result<zx::time> result) {
      EXPECT_TRUE(result.is_ok()) << result.status_string();
      ASSERT_TRUE(ring_buffer.start_time.has_value());
      EXPECT_EQ(*result, *ring_buffer.start_time);
    });

    RunLoopUntilIdle();
    ASSERT_TRUE(ring_buffer.start_time.has_value());
    EXPECT_GT(ring_buffer.start_time->get(), now);
    EXPECT_TRUE(RingBufferIsStarted(device, element_id));
  }

  void StopAndExpectValid(const std::shared_ptr<Device>& device, ElementId element_id) {
    ASSERT_TRUE(RingBufferIsStarted(device, element_id));

    device->StopRingBuffer(element_id, [](zx_status_t result) { EXPECT_EQ(result, ZX_OK); });

    RunLoopUntilIdle();
    auto& ring_buffer = device->ring_buffer_map_.find(element_id)->second;
    ASSERT_FALSE(ring_buffer.start_time.has_value());
    EXPECT_TRUE(RingBufferIsStopped(device, element_id));
  }

 private:
  std::shared_ptr<FakeStreamConfig> MakeFakeStreamConfig(bool is_input = false) {
    auto stream_config_endpoints = fidl::Endpoints<fuchsia_hardware_audio::StreamConfig>::Create();
    auto fake_stream = std::make_shared<FakeStreamConfig>(
        stream_config_endpoints.server.TakeChannel(), stream_config_endpoints.client.TakeChannel(),
        dispatcher());
    fake_stream->set_is_input(is_input);
    return fake_stream;
  }
};

}  // namespace media_audio

#endif  // SRC_MEDIA_RVICES_DEVICE_REGISTRY_DEVICE_UNITTEST_H_
