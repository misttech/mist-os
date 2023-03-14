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
#include "src/media/audio/services/device_registry/testing/fake_audio_driver.h"

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

// Validate that a fake driver with default values is initialized successfully.
TEST_F(DeviceTest, Initialization) {
  InitializeDeviceForFakeDriver();
  EXPECT_TRUE(InInitializedState(device_));

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);

  EXPECT_EQ(fake_device_presence_watcher_->on_ready_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_error_count(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_count(), 0u);

  fake_device_presence_watcher_.reset();
}

// Validate that a driver's dropping the StreamConfig causes a DeviceIsRemoved notification.
TEST_F(DeviceTest, StreamConfigDisconnect) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  ASSERT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  ASSERT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);

  ASSERT_EQ(fake_device_presence_watcher_->on_ready_count(), 1u);
  ASSERT_EQ(fake_device_presence_watcher_->on_error_count(), 0u);
  ASSERT_EQ(fake_device_presence_watcher_->on_removal_count(), 0u);

  FakeDriverDropStreamConfig();
  RunLoopUntilIdle();

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);

  EXPECT_EQ(fake_device_presence_watcher_->on_ready_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_error_count(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_from_ready_count(), 1u);
}

// Validate that a GetHealthState response of an empty struct is considered "healthy".
TEST_F(DeviceTest, EmptyHealthResponse) {
  fake_driver_->set_health_state(std::nullopt);
  InitializeDeviceForFakeDriver();
  EXPECT_TRUE(InInitializedState(device_));

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);
}

// TODO(fxbug.dev/117826): manufacturer and product strings that are 256 chars long

// TODO(fxbug.dev/117826): unittest ValidateStreamProperties

// TODO(fxbug.dev/117826): unittest ValidateSupportedFormats

TEST_F(DeviceTest, DistinctTokenIds) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));

  // Set up a second, entirely distinct fake device.
  zx::channel server_end, client_end;
  ASSERT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));

  auto fake_driver2 =
      std::make_unique<FakeAudioDriver>(std::move(server_end), std::move(client_end), dispatcher());
  fake_driver2->set_is_input(true);

  auto device2 = InitializeDeviceForFakeDriver(fake_driver2);
  EXPECT_TRUE(InInitializedState(device2));

  EXPECT_NE(device_->token_id(), device2->token_id());

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 2u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);
}

TEST_F(DeviceTest, DefaultClock) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));

  EXPECT_EQ(device_clock()->domain(), fuchsia_hardware_audio::kClockDomainMonotonic);
  EXPECT_TRUE(device_clock()->IdenticalToMonotonicClock());
  EXPECT_FALSE(device_clock()->adjustable());

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);
}

TEST_F(DeviceTest, ClockInOtherDomain) {
  const uint32_t kNonMonotonicClockDomain = fuchsia_hardware_audio::kClockDomainMonotonic + 1;
  fake_driver_->set_clock_domain(kNonMonotonicClockDomain);
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));

  EXPECT_EQ(device_clock()->domain(), kNonMonotonicClockDomain);
  EXPECT_TRUE(device_clock()->IdenticalToMonotonicClock());
  EXPECT_TRUE(device_clock()->adjustable());

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);
}

// TODO(fxbug.dev/117826): unittest ValidateGainState

// TODO(fxbug.dev/117826): unittest ValidatePlugState

TEST_F(DeviceTest, CreateDeviceInfo) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  auto info = GetDeviceInfo();

  EXPECT_TRUE(info.device_type());
  EXPECT_EQ(*info.device_type(), fuchsia_audio_device::DeviceType::kOutput);

  EXPECT_TRUE(info.clock_domain());
  EXPECT_EQ(*info.clock_domain(), fuchsia_hardware_audio::kClockDomainMonotonic);
}

// TODO(fxbug.dev/117826): unittest ValidateDeviceInfo

// TODO(fxbug.dev/117826): unittest TranslateFormatSets

// TODO(fxbug.dev/117826): unittest RetrieveCurrentlyPermittedFormats

TEST_F(DeviceTest, SupportedDriverFormatForClientFormat) {
  fake_driver_->set_frame_rates(0, {48000, 48001});
  fake_driver_->set_valid_bits_per_sample(0, {12, 15, 20});
  fake_driver_->set_bytes_per_sample(0, {2, 4});
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));

  auto valid_bits = ExpectFormatMatch(device_, fuchsia_audio::SampleType::kInt16, 2, 48001);
  EXPECT_EQ(valid_bits, 15u);

  valid_bits = ExpectFormatMatch(device_, fuchsia_audio::SampleType::kInt32, 2, 48000);
  EXPECT_EQ(valid_bits, 20u);
}

// This tests the driver's ability to inform its ObserverNotify of initial gain state.
TEST_F(DeviceTest, InitialGainState) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  ASSERT_TRUE(AddObserver(device_));

  RunLoopUntilIdle();
  auto gain_state = DeviceGainState(device_);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());
  ASSERT_TRUE(notify_->gain_state()) << "ObserverNotify was not notified of initial gain state";
  ASSERT_TRUE(notify_->gain_state()->gain_db());
  EXPECT_EQ(*notify_->gain_state()->gain_db(), 0.0f);
  EXPECT_FALSE(notify_->gain_state()->muted().value_or(false));
  EXPECT_FALSE(notify_->gain_state()->agc_enabled().value_or(false));
}

// This tests the driver's ability to inform its ObserverNotify of initial plug state.
TEST_F(DeviceTest, InitialPlugState) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  ASSERT_TRUE(AddObserver(device_));

  RunLoopUntilIdle();
  EXPECT_TRUE(DevicePluggedState(device_));
  ASSERT_TRUE(notify_->plug_state());
  EXPECT_EQ(notify_->plug_state()->first, fuchsia_audio_device::PlugState::kPlugged);
  EXPECT_EQ(notify_->plug_state()->second, zx::time(0));
}

// This tests the driver's ability to originate gain changes, such as from hardware buttons. It also
// validates that gain notifications are delivered through ControlNotify (not just ObserverNotify).
TEST_F(DeviceTest, DynamicGainUpdate) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  ASSERT_TRUE(SetControl(device_));

  RunLoopUntilIdle();
  auto gain_state = DeviceGainState(device_);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());
  EXPECT_EQ(*notify_->gain_state()->gain_db(), 0.0f);
  EXPECT_FALSE(notify_->gain_state()->muted().value_or(false));
  EXPECT_FALSE(notify_->gain_state()->agc_enabled().value_or(false));
  notify_->gain_state() = std::nullopt;

  constexpr float kNewGainDb = -2.0f;
  fake_driver_->InjectGainChange({{
      .muted = true,
      .agc_enabled = true,
      .gain_db = kNewGainDb,
  }});

  RunLoopUntilIdle();
  gain_state = DeviceGainState(device_);
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

// This tests the driver's ability to originate plug changes, such as from jack detection. It also
// validates that plug notifications are delivered through ControlNotify (not just ObserverNotify).
TEST_F(DeviceTest, DynamicPlugUpdate) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  ASSERT_TRUE(SetControl(device_));

  RunLoopUntilIdle();
  EXPECT_TRUE(DevicePluggedState(device_));
  ASSERT_TRUE(notify_->plug_state());
  EXPECT_EQ(notify_->plug_state()->first, fuchsia_audio_device::PlugState::kPlugged);
  EXPECT_EQ(notify_->plug_state()->second, zx::time(0));
  notify_->plug_state() = std::nullopt;

  auto unplug_time = zx::clock::get_monotonic();
  fake_driver_->InjectPlugChange(false, unplug_time);
  RunLoopUntilIdle();
  EXPECT_FALSE(DevicePluggedState(device_));
  ASSERT_TRUE(notify_->plug_state());
  EXPECT_EQ(notify_->plug_state()->first, fuchsia_audio_device::PlugState::kUnplugged);
  EXPECT_EQ(notify_->plug_state()->second, unplug_time);
}

TEST_F(DeviceTest, Observer) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));

  EXPECT_TRUE(AddObserver(device_));
}

TEST_F(DeviceTest, Control) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  ASSERT_TRUE(SetControl(device_));

  EXPECT_TRUE(DropControl(device_));
}

// This tests the ability to set gain to the driver, such as from GUI volume controls.
TEST_F(DeviceTest, SetGain) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  ASSERT_TRUE(SetControl(device_));

  RunLoopUntilIdle();
  auto gain_state = DeviceGainState(device_);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());
  ASSERT_TRUE(notify_->gain_state()) << "ObserverNotify was not notified of initial gain state";
  EXPECT_EQ(*notify_->gain_state()->gain_db(), 0.0f);
  EXPECT_FALSE(notify_->gain_state()->muted().value_or(false));
  EXPECT_FALSE(notify_->gain_state()->agc_enabled().value_or(false));
  notify_->gain_state() = std::nullopt;

  constexpr float kNewGainDb = -2.0f;
  EXPECT_TRUE(SetDeviceGain({{
      .muted = true,
      .agc_enabled = true,
      .gain_db = kNewGainDb,
  }}));

  RunLoopUntilIdle();
  gain_state = DeviceGainState(device_);
  EXPECT_EQ(*gain_state.gain_db(), kNewGainDb);
  EXPECT_TRUE(*gain_state.muted());
  EXPECT_TRUE(*gain_state.agc_enabled());

  ASSERT_TRUE(notify_->gain_state() && notify_->gain_state()->gain_db() &&
              notify_->gain_state()->muted() && notify_->gain_state()->agc_enabled());
  EXPECT_EQ(*notify_->gain_state()->gain_db(), kNewGainDb);
  EXPECT_TRUE(notify_->gain_state()->muted().value_or(false));        // Must be present and true.
  EXPECT_TRUE(notify_->gain_state()->agc_enabled().value_or(false));  // Must be present and true.
}

// Validate that Device can open the driver's RingBuffer FIDL channel.
TEST_F(DeviceTest, RingBufferCreation) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  fake_driver_->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device_));

  ConnectToRingBufferAndExpectValidClient();
}

TEST_F(DeviceTest, RingBufferProperties) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  fake_driver_->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device_));
  ConnectToRingBufferAndExpectValidClient();

  GetRingBufferProperties();
}

TEST_F(DeviceTest, DelayInfo) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  fake_driver_->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device_));
  ConnectToRingBufferAndExpectValidClient();

  // Validate that the device received the expected values.
  RetrieveDelayInfoAndExpect(0, std::nullopt);

  // Validate that the ControlNotify was sent the expected values.
  ASSERT_TRUE(notify_->delay_info()) << "ControlNotify was not notified of initial delay info";
  ASSERT_TRUE(notify_->delay_info()->internal_delay());
  EXPECT_FALSE(notify_->delay_info()->external_delay());
  EXPECT_EQ(*notify_->delay_info()->internal_delay(), 0);
}

// TODO(fxbug.dev/117826): Unittest CalculateRequiredRingBufferSizes

TEST_F(DeviceTest, RingBufferVmo) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  fake_driver_->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device_));
  ConnectToRingBufferAndExpectValidClient();

  GetDriverVmoAndExpectValid();
}

TEST_F(DeviceTest, CreateRingBuffer) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  fake_driver_->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device_));

  auto connected_to_ring_buffer_fidl =
      device_->CreateRingBuffer(kDefaultRingBufferFormat, 2000, [](Device::RingBufferInfo info) {
        EXPECT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);
}

TEST_F(DeviceTest, SetActiveChannels) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  fake_driver_->AllocateRingBuffer(8192);
  fake_driver_->set_active_channels_supported(true);
  ASSERT_TRUE(SetControl(device_));
  ConnectToRingBufferAndExpectValidClient();

  ExpectActiveChannels(0x0003);

  SetActiveChannelsAndExpect(0x0002);
}

// TODO(fxbug.dev/117826): SetActiveChannel no change => no callback (no change in set_time)

TEST_F(DeviceTest, BasicStartAndStop) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  fake_driver_->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device_));

  auto connected_to_ring_buffer_fidl =
      device_->CreateRingBuffer(kDefaultRingBufferFormat, 2000, [](Device::RingBufferInfo info) {
        EXPECT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);

  ExpectRingBufferReady();

  StartAndExpectValid();
  StopAndExpectValid();
}

TEST_F(DeviceTest, InitialDelayReceivedDuringCreateRingBuffer) {
  InitializeDeviceForFakeDriver();
  fake_driver_->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device_));

  auto created_ring_buffer = false;
  auto connected_to_ring_buffer_fidl = device_->CreateRingBuffer(
      kDefaultRingBufferFormat, 2000,
      [&created_ring_buffer](Device::RingBufferInfo info) { created_ring_buffer = true; });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);
  RunLoopUntilIdle();

  // Validate that the device received the expected values.
  EXPECT_TRUE(created_ring_buffer);
  ASSERT_TRUE(DeviceDelayInfo(device_));
  ASSERT_TRUE(DeviceDelayInfo(device_)->internal_delay());
  EXPECT_FALSE(DeviceDelayInfo(device_)->external_delay());
  EXPECT_EQ(*DeviceDelayInfo(device_)->internal_delay(), 0);

  // Validate that the ControlNotify was sent the expected values.
  ASSERT_TRUE(notify_->delay_info()) << "ControlNotify was not notified of initial delay info";
  ASSERT_TRUE(notify_->delay_info()->internal_delay());
  EXPECT_FALSE(notify_->delay_info()->external_delay());
  EXPECT_EQ(*notify_->delay_info()->internal_delay(), 0);
}

TEST_F(DeviceTest, DynamicDelayInfo) {
  InitializeDeviceForFakeDriver();
  fake_driver_->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device_));

  auto created_ring_buffer = false;
  auto connected_to_ring_buffer_fidl = device_->CreateRingBuffer(
      kDefaultRingBufferFormat, 2000,
      [&created_ring_buffer](Device::RingBufferInfo info) { created_ring_buffer = true; });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);
  RunLoopUntilIdle();

  EXPECT_TRUE(created_ring_buffer);
  ASSERT_TRUE(notify_->delay_info()) << "ControlNotify was not notified of initial delay info";
  ASSERT_TRUE(notify_->delay_info()->internal_delay());
  EXPECT_FALSE(notify_->delay_info()->external_delay());
  EXPECT_EQ(*notify_->delay_info()->internal_delay(), 0);
  notify_->delay_info() = std::nullopt;

  RunLoopUntilIdle();
  EXPECT_FALSE(notify_->delay_info());

  fake_driver_->InjectDelayUpdate(zx::nsec(123'456), zx::nsec(654'321));
  RunLoopUntilIdle();
  ASSERT_TRUE(DeviceDelayInfo(device_));
  ASSERT_TRUE(DeviceDelayInfo(device_)->internal_delay());
  ASSERT_TRUE(DeviceDelayInfo(device_)->external_delay());
  EXPECT_EQ(*DeviceDelayInfo(device_)->internal_delay(), 123'456);
  EXPECT_EQ(*DeviceDelayInfo(device_)->external_delay(), 654'321);

  ASSERT_TRUE(notify_->delay_info()) << "ControlNotify was not notified with updated delay info";
  ASSERT_TRUE(notify_->delay_info()->internal_delay());
  ASSERT_TRUE(notify_->delay_info()->external_delay());
  EXPECT_EQ(*notify_->delay_info()->internal_delay(), 123'456);
  EXPECT_EQ(*notify_->delay_info()->external_delay(), 654'321);
}

TEST_F(DeviceTest, SetActiveChannelsDuringCreateRingBuffer) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  fake_driver_->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device_));

  auto connected_to_ring_buffer_fidl =
      device_->CreateRingBuffer(kDefaultRingBufferFormat, 2000, [](Device::RingBufferInfo info) {
        EXPECT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);

  ExpectRingBufferReady();
  EXPECT_THAT(device_->supports_set_active_channels(), Optional(true));
}

TEST_F(DeviceTest, SetActiveChannelsUnsupportedDuringCreateRingBuffer) {
  InitializeDeviceForFakeDriver();
  fake_driver_->set_active_channels_supported(false);
  ASSERT_TRUE(InInitializedState(device_));
  fake_driver_->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device_));

  auto connected_to_ring_buffer_fidl =
      device_->CreateRingBuffer(kDefaultRingBufferFormat, 2000, [](Device::RingBufferInfo info) {
        EXPECT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);

  ExpectRingBufferReady();
  EXPECT_THAT(device_->supports_set_active_channels(), Optional(false));
}

}  // namespace media_audio
