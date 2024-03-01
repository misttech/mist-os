// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/device.h"

#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fuchsia/hardware/audio/cpp/fidl.h>
#include <lib/zx/clock.h>
#include <zircon/errors.h>

#include <optional>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/device_unittest.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"

namespace media_audio {

using ::testing::Optional;

class DeviceTest : public DeviceTestBase {
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
  // Accessor for a Device private member.
  static const std::optional<fuchsia_hardware_audio::DelayInfo>& DeviceDelayInfo(
      std::shared_ptr<Device> device) {
    return device->delay_info_;
  }
};
class StreamConfigTest : public DeviceTest {};

// Validate that a fake stream_config with default values is initialized successfully.
TEST_F(StreamConfigTest, Initialization) {
  auto device = InitializeDeviceForFakeStreamConfig(MakeFakeStreamConfigOutput());
  EXPECT_TRUE(InInitializedState(device));

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);

  EXPECT_EQ(fake_device_presence_watcher_->on_ready_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_error_count(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_count(), 0u);

  fake_device_presence_watcher_.reset();
}

// Validate that a driver's dropping the StreamConfig causes a DeviceIsRemoved notification.
TEST_F(StreamConfigTest, Disconnect) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  ASSERT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);

  ASSERT_EQ(fake_device_presence_watcher_->on_ready_count(), 1u);
  ASSERT_EQ(fake_device_presence_watcher_->on_error_count(), 0u);
  ASSERT_EQ(fake_device_presence_watcher_->on_removal_count(), 0u);

  fake_stream_config->DropStreamConfig();
  RunLoopUntilIdle();

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);

  EXPECT_EQ(fake_device_presence_watcher_->on_ready_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_error_count(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_from_ready_count(), 1u);
}

// Validate that a GetHealthState response of an empty struct is considered "healthy".
TEST_F(StreamConfigTest, EmptyHealthResponse) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  fake_stream_config->set_health_state(std::nullopt);
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  EXPECT_TRUE(InInitializedState(device));

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);
}

TEST_F(DeviceTest, DistinctTokenIds) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));

  // Set up a second, entirely distinct fake device.
  zx::channel server_end, client_end;
  ASSERT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));

  auto fake_driver2 = std::make_unique<FakeStreamConfig>(std::move(server_end),
                                                         std::move(client_end), dispatcher());
  fake_driver2->set_is_input(true);

  auto device2 = InitializeDeviceForFakeStreamConfig(fake_driver2);
  EXPECT_TRUE(InInitializedState(device2));

  EXPECT_NE(device->token_id(), device2->token_id());

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 2u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);
}

TEST_F(StreamConfigTest, DefaultClock) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));

  EXPECT_EQ(device_clock(device)->domain(), fuchsia_hardware_audio::kClockDomainMonotonic);
  EXPECT_TRUE(device_clock(device)->IdenticalToMonotonicClock());
  EXPECT_FALSE(device_clock(device)->adjustable());

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);
}

TEST_F(StreamConfigTest, ClockInOtherDomain) {
  const uint32_t kNonMonotonicClockDomain = fuchsia_hardware_audio::kClockDomainMonotonic + 1;
  auto fake_stream_config = MakeFakeStreamConfigInput();
  fake_stream_config->set_clock_domain(kNonMonotonicClockDomain);
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));

  EXPECT_EQ(device_clock(device)->domain(), kNonMonotonicClockDomain);
  EXPECT_TRUE(device_clock(device)->IdenticalToMonotonicClock());
  EXPECT_TRUE(device_clock(device)->adjustable());

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);
}

TEST_F(StreamConfigTest, DeviceInfo) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  auto info = GetDeviceInfo(device);

  EXPECT_TRUE(info.token_id());
  EXPECT_TRUE(info.device_type());
  EXPECT_EQ(*info.device_type(), fuchsia_audio_device::DeviceType::kOutput);
  EXPECT_TRUE(info.device_name());
  // manufacturer is optional, but it can't be an empty string
  EXPECT_TRUE(!info.manufacturer().has_value() || !info.manufacturer()->empty());
  // product is optional, but it can't be an empty string
  EXPECT_TRUE(!info.product().has_value() || !info.product()->empty());
  // unique_instance_id is optional
  EXPECT_TRUE(info.gain_caps());
  EXPECT_TRUE(info.plug_detect_caps());
  EXPECT_TRUE(info.clock_domain());
  EXPECT_EQ(*info.clock_domain(), fuchsia_hardware_audio::kClockDomainMonotonic);
}

TEST_F(StreamConfigTest, SupportedDriverFormatForClientFormat) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  fake_stream_config->set_frame_rates(0, {48000, 48001});
  fake_stream_config->set_valid_bits_per_sample(0, {12, 15, 20});
  fake_stream_config->set_bytes_per_sample(0, {2, 4});
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));

  auto valid_bits = ExpectFormatMatch(device, fuchsia_audio::SampleType::kInt16, 2, 48001);
  EXPECT_EQ(valid_bits, 15u);

  valid_bits = ExpectFormatMatch(device, fuchsia_audio::SampleType::kInt32, 2, 48000);
  EXPECT_EQ(valid_bits, 20u);
}

TEST_F(StreamConfigTest, Observer) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));

  EXPECT_TRUE(AddObserver(device));
}

TEST_F(StreamConfigTest, Control) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  EXPECT_TRUE(DropControl(device));
}

// This tests the driver's ability to inform its ObserverNotify of initial gain state.
TEST_F(StreamConfigTest, InitialGainState) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  auto gain_state = DeviceGainState(device);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());
  ASSERT_TRUE(notify_->gain_state()) << "ObserverNotify was not notified of initial gain state";
  ASSERT_TRUE(notify_->gain_state()->gain_db());
  EXPECT_EQ(*notify_->gain_state()->gain_db(), 0.0f);
  EXPECT_FALSE(notify_->gain_state()->muted().value_or(false));
  EXPECT_FALSE(notify_->gain_state()->agc_enabled().value_or(false));
}

// This tests the driver's ability to originate gain changes, such as from hardware buttons. It also
// validates that gain notifications are delivered through ControlNotify (not just ObserverNotify).
TEST_F(StreamConfigTest, DynamicGainUpdate) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  RunLoopUntilIdle();
  auto gain_state = DeviceGainState(device);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());
  EXPECT_EQ(*notify_->gain_state()->gain_db(), 0.0f);
  EXPECT_FALSE(notify_->gain_state()->muted().value_or(false));
  EXPECT_FALSE(notify_->gain_state()->agc_enabled().value_or(false));
  notify_->gain_state().reset();

  constexpr float kNewGainDb = -2.0f;
  fake_stream_config->InjectGainChange({{
      .muted = true,
      .agc_enabled = true,
      .gain_db = kNewGainDb,
  }});

  RunLoopUntilIdle();
  gain_state = DeviceGainState(device);
  EXPECT_EQ(*gain_state.gain_db(), kNewGainDb);
  EXPECT_TRUE(*gain_state.muted());
  EXPECT_TRUE(*gain_state.agc_enabled());

  ASSERT_TRUE(notify_->gain_state());
  ASSERT_TRUE(notify_->gain_state()->gain_db());
  ASSERT_TRUE(notify_->gain_state()->muted());
  ASSERT_TRUE(notify_->gain_state()->agc_enabled());
  EXPECT_EQ(*notify_->gain_state()->gain_db(), kNewGainDb);
  EXPECT_TRUE(*notify_->gain_state()->muted());
  EXPECT_TRUE(*notify_->gain_state()->agc_enabled());
}

// This tests the ability to set gain to the driver, such as from GUI volume controls.
TEST_F(StreamConfigTest, SetGain) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  RunLoopUntilIdle();
  auto gain_state = DeviceGainState(device);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());
  ASSERT_TRUE(notify_->gain_state()) << "ObserverNotify was not notified of initial gain state";
  EXPECT_EQ(*notify_->gain_state()->gain_db(), 0.0f);
  EXPECT_FALSE(notify_->gain_state()->muted().value_or(false));
  EXPECT_FALSE(notify_->gain_state()->agc_enabled().value_or(false));
  notify_->gain_state().reset();

  constexpr float kNewGainDb = -2.0f;
  EXPECT_TRUE(SetDeviceGain(device, {{
                                        .muted = true,
                                        .agc_enabled = true,
                                        .gain_db = kNewGainDb,
                                    }}));

  RunLoopUntilIdle();
  gain_state = DeviceGainState(device);
  EXPECT_EQ(*gain_state.gain_db(), kNewGainDb);
  EXPECT_TRUE(*gain_state.muted());
  EXPECT_TRUE(*gain_state.agc_enabled());

  ASSERT_TRUE(notify_->gain_state() && notify_->gain_state()->gain_db() &&
              notify_->gain_state()->muted() && notify_->gain_state()->agc_enabled());
  EXPECT_EQ(*notify_->gain_state()->gain_db(), kNewGainDb);
  EXPECT_TRUE(notify_->gain_state()->muted().value_or(false));        // Must be present and true.
  EXPECT_TRUE(notify_->gain_state()->agc_enabled().value_or(false));  // Must be present and true.
}

// This tests the driver's ability to inform its ObserverNotify of initial plug state.
TEST_F(StreamConfigTest, InitialPlugState) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  EXPECT_TRUE(DevicePluggedState(device));
  ASSERT_TRUE(notify_->plug_state());
  EXPECT_EQ(notify_->plug_state()->first, fuchsia_audio_device::PlugState::kPlugged);
  EXPECT_EQ(notify_->plug_state()->second, zx::time(0));
}

TEST_F(StreamConfigTest, DynamicPlugUpdate) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  RunLoopUntilIdle();
  EXPECT_TRUE(DevicePluggedState(device));
  ASSERT_TRUE(notify_->plug_state());
  EXPECT_EQ(notify_->plug_state()->first, fuchsia_audio_device::PlugState::kPlugged);
  EXPECT_EQ(notify_->plug_state()->second, zx::time(0));
  notify_->plug_state().reset();

  auto unplug_time = zx::clock::get_monotonic();
  fake_stream_config->InjectPlugChange(false, unplug_time);
  RunLoopUntilIdle();
  EXPECT_FALSE(DevicePluggedState(device));
  ASSERT_TRUE(notify_->plug_state());
  EXPECT_EQ(notify_->plug_state()->first, fuchsia_audio_device::PlugState::kUnplugged);
  EXPECT_EQ(notify_->plug_state()->second, unplug_time);
}

// TODO(https://fxbug.dev/42069012): unittest RetrieveCurrentlyPermittedFormats

// Validate that Device can open the driver's RingBuffer FIDL channel.
TEST_F(StreamConfigTest, CreateRingBuffer) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto connected_to_ring_buffer_fidl =
      device->CreateRingBuffer(kDefaultRingBufferFormat, 2000, [](Device::RingBufferInfo info) {
        EXPECT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);
}

TEST_F(StreamConfigTest, RingBufferProperties) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));
  ConnectToRingBufferAndExpectValidClient(device);

  GetRingBufferProperties(device);
}

// TODO(https://fxbug.dev/42069012): Unittest CalculateRequiredRingBufferSizes

TEST_F(StreamConfigTest, RingBufferGetVmo) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));
  ConnectToRingBufferAndExpectValidClient(device);

  GetDriverVmoAndExpectValid(device);
}

TEST_F(StreamConfigTest, BasicStartAndStop) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto connected_to_ring_buffer_fidl =
      device->CreateRingBuffer(kDefaultRingBufferFormat, 2000, [](Device::RingBufferInfo info) {
        EXPECT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);

  ExpectRingBufferReady(device);

  StartAndExpectValid(device);
  StopAndExpectValid(device);
}

TEST_F(StreamConfigTest, InitialDelay) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto created_ring_buffer = false;
  auto connected_to_ring_buffer_fidl = device->CreateRingBuffer(
      kDefaultRingBufferFormat, 2000,
      [&created_ring_buffer](Device::RingBufferInfo info) { created_ring_buffer = true; });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);
  RunLoopUntilIdle();

  // Validate that the device received the expected values.
  EXPECT_TRUE(created_ring_buffer);
  ASSERT_TRUE(DeviceDelayInfo(device));
  ASSERT_TRUE(DeviceDelayInfo(device)->internal_delay());
  EXPECT_FALSE(DeviceDelayInfo(device)->external_delay());
  EXPECT_EQ(*DeviceDelayInfo(device)->internal_delay(), 0);

  // Validate that the ControlNotify was sent the expected values.
  ASSERT_TRUE(notify_->delay_info()) << "ControlNotify was not notified of initial delay info";
  ASSERT_TRUE(notify_->delay_info()->internal_delay());
  EXPECT_FALSE(notify_->delay_info()->external_delay());
  EXPECT_EQ(*notify_->delay_info()->internal_delay(), 0);
}

TEST_F(StreamConfigTest, DynamicDelay) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto created_ring_buffer = false;
  auto connected_to_ring_buffer_fidl = device->CreateRingBuffer(
      kDefaultRingBufferFormat, 2000,
      [&created_ring_buffer](Device::RingBufferInfo info) { created_ring_buffer = true; });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);
  RunLoopUntilIdle();

  EXPECT_TRUE(created_ring_buffer);
  ASSERT_TRUE(notify_->delay_info()) << "ControlNotify was not notified of initial delay info";
  ASSERT_TRUE(notify_->delay_info()->internal_delay());
  EXPECT_FALSE(notify_->delay_info()->external_delay());
  EXPECT_EQ(*notify_->delay_info()->internal_delay(), 0);
  notify_->delay_info().reset();

  RunLoopUntilIdle();
  EXPECT_FALSE(notify_->delay_info());

  fake_stream_config->InjectDelayUpdate(zx::nsec(123'456), zx::nsec(654'321));
  RunLoopUntilIdle();
  ASSERT_TRUE(DeviceDelayInfo(device));
  ASSERT_TRUE(DeviceDelayInfo(device)->internal_delay());
  ASSERT_TRUE(DeviceDelayInfo(device)->external_delay());
  EXPECT_EQ(*DeviceDelayInfo(device)->internal_delay(), 123'456);
  EXPECT_EQ(*DeviceDelayInfo(device)->external_delay(), 654'321);

  ASSERT_TRUE(notify_->delay_info()) << "ControlNotify was not notified with updated delay info";
  ASSERT_TRUE(notify_->delay_info()->internal_delay());
  ASSERT_TRUE(notify_->delay_info()->external_delay());
  EXPECT_EQ(*notify_->delay_info()->internal_delay(), 123'456);
  EXPECT_EQ(*notify_->delay_info()->external_delay(), 654'321);
}

TEST_F(StreamConfigTest, ReportsThatItSupportsSetActiveChannels) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);

  ASSERT_TRUE(InInitializedState(device));
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto connected_to_ring_buffer_fidl =
      device->CreateRingBuffer(kDefaultRingBufferFormat, 2000, [](Device::RingBufferInfo info) {
        EXPECT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);

  ExpectRingBufferReady(device);
  EXPECT_THAT(device->supports_set_active_channels(), Optional(true));
}

TEST_F(StreamConfigTest, ReportsThatItDoesNotSupportSetActiveChannels) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  fake_stream_config->set_active_channels_supported(false);
  ASSERT_TRUE(InInitializedState(device));
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto connected_to_ring_buffer_fidl =
      device->CreateRingBuffer(kDefaultRingBufferFormat, 2000, [](Device::RingBufferInfo info) {
        EXPECT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);

  ExpectRingBufferReady(device);
  EXPECT_THAT(device->supports_set_active_channels(), Optional(false));
}

TEST_F(StreamConfigTest, SetActiveChannels) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  fake_stream_config->AllocateRingBuffer(8192);
  fake_stream_config->set_active_channels_supported(true);
  ASSERT_TRUE(SetControl(device));
  ConnectToRingBufferAndExpectValidClient(device);

  ExpectActiveChannels(device, 0x0003);

  SetActiveChannelsAndExpect(device, 0x0002);
}

// TODO(https://fxbug.dev/42069012): SetActiveChannel no change => no callback (no set_time change).

}  // namespace media_audio
