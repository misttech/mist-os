// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/device.h"

#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <fidl/fuchsia.mediastreams/cpp/common_types.h>
#include <lib/zx/clock.h>
#include <zircon/system/public/zircon/errors.h>

#include <optional>

#include <gtest/gtest.h>

#include "fuchsia/hardware/audio/cpp/fidl.h"
#include "src/media/audio/services/device_registry/device_unittest.h"
#include "src/media/audio/services/device_registry/testing/fake_audio_driver.h"

namespace media_audio {

class DeviceTest : public DeviceTestBase {};

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

TEST_F(DeviceTest, EmptyHealthResponse) {
  fake_driver_->set_health_state(std::nullopt);
  InitializeDeviceForFakeDriver();
  EXPECT_TRUE(InInitializedState(device_));

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);
}

// TODO: manufacturer and product strings that are 256 chars long

// TODO: unittest ValidateStreamProperties

// TODO: unittest ValidateSupportedFormats

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

// TODO: unittest ValidateGainState

// TODO: unittest ValidatePlugState

TEST_F(DeviceTest, CreateDeviceInfo) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  auto info = GetDeviceInfo();

  EXPECT_TRUE(info.device_type());
  EXPECT_EQ(*info.device_type(), fuchsia_audio_device::DeviceType::kOutput);

  EXPECT_TRUE(info.clock_domain());
  EXPECT_EQ(*info.clock_domain(), fuchsia_hardware_audio::kClockDomainMonotonic);
}

// TODO: unittest ValidateDeviceInfo

// TODO: unittest RetrieveCurrentlyPermittedFormats

// This tests the ability to change gain from the driver itself, such as from hardware buttons.
TEST_F(DeviceTest, DynamicGainUpdate) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));

  RunLoopUntilIdle();
  auto gain_state = DeviceGainState(device_);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());

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
}

TEST_F(DeviceTest, DynamicPlugUpdate) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  EXPECT_TRUE(DevicePluggedState(device_));

  fake_driver_->InjectPlugChange(false, zx::clock::get_monotonic());
  RunLoopUntilIdle();
  EXPECT_FALSE(DevicePluggedState(device_));
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
}

}  // namespace media_audio
