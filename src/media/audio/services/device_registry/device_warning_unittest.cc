// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/device_unittest.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"

namespace media_audio {

class DeviceWarningTest : public DeviceTestBase {};

// TODO(https://fxbug.dev/42069012): test non-compliant driver behavior (e.g. min_gain>max_gain).

TEST_F(DeviceWarningTest, DeviceUnhealthy) {
  fake_driver_->set_health_state(false);
  InitializeDeviceForFakeDriver();

  EXPECT_TRUE(HasError(device_));
  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 1u);

  EXPECT_EQ(fake_device_presence_watcher_->on_ready_count(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->on_error_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_count(), 0u);
}

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test: device becomes
//   unhealthy before any Device method. Expect method-specific failure + State::Error notif.

TEST_F(DeviceWarningTest, UnhealthyDeviceRemoved) {
  fake_driver_->set_health_state(false);
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(HasError(device_));
  ASSERT_EQ(fake_device_presence_watcher_->ready_devices().size(), 0u);
  ASSERT_EQ(fake_device_presence_watcher_->error_devices().size(), 1u);

  ASSERT_EQ(fake_device_presence_watcher_->on_ready_count(), 0u);
  ASSERT_EQ(fake_device_presence_watcher_->on_error_count(), 1u);
  ASSERT_EQ(fake_device_presence_watcher_->on_removal_count(), 0u);

  fake_driver_->DropStreamConfig();
  RunLoopUntilIdle();

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);

  EXPECT_EQ(fake_device_presence_watcher_->on_ready_count(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->on_error_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_from_error_count(), 1u);
}

TEST_F(DeviceWarningTest, DeviceAlreadyControlled) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));

  EXPECT_TRUE(SetControl(device_));

  EXPECT_FALSE(SetControl(device_));
  EXPECT_TRUE(IsControlled(device_));
}

TEST_F(DeviceWarningTest, UnhealthyDeviceCannotBeControlled) {
  fake_driver_->set_health_state(false);
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(HasError(device_));

  EXPECT_FALSE(SetControl(device_));
}

TEST_F(DeviceWarningTest, SetGainWithoutControl) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));

  RunLoopUntilIdle();
  auto gain_state = DeviceGainState(device_);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());

  constexpr float kNewGainDb = -2.0f;
  EXPECT_FALSE(SetDeviceGain({{
      .muted = true,
      .agc_enabled = true,
      .gain_db = kNewGainDb,
  }}));

  RunLoopUntilIdle();
  gain_state = DeviceGainState(device_);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());
}

TEST_F(DeviceWarningTest, NoMatchForSupportedDriverFormatForClientFormat) {
  fake_driver_->set_frame_rates(0, {48000});
  fake_driver_->set_valid_bits_per_sample(0, {12, 15, 20});
  fake_driver_->set_bytes_per_sample(0, {2, 4});
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));

  ExpectNoFormatMatch(device_, fuchsia_audio::SampleType::kInt16, 2, 47999);
  ExpectNoFormatMatch(device_, fuchsia_audio::SampleType::kInt16, 2, 48001);
  ExpectNoFormatMatch(device_, fuchsia_audio::SampleType::kInt16, 1, 48000);
  ExpectNoFormatMatch(device_, fuchsia_audio::SampleType::kInt16, 3, 48000);
  ExpectNoFormatMatch(device_, fuchsia_audio::SampleType::kUint8, 2, 48000);
  ExpectNoFormatMatch(device_, fuchsia_audio::SampleType::kFloat32, 2, 48000);
  ExpectNoFormatMatch(device_, fuchsia_audio::SampleType::kFloat64, 2, 48000);
}

TEST_F(DeviceWarningTest, CannotAddSameObserverTwice) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  ASSERT_TRUE(AddObserver(device_));

  EXPECT_FALSE(AddObserver(device_));
}

TEST_F(DeviceWarningTest, CannotObserveUnhealthyDevice) {
  fake_driver_->set_health_state(false);
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(HasError(device_));

  EXPECT_FALSE(AddObserver(device_));
}

TEST_F(DeviceWarningTest, CannotSetControlTwice) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  ASSERT_TRUE(SetControl(device_));

  EXPECT_FALSE(SetControl(device_));
}

TEST_F(DeviceWarningTest, CannotDropUnknownControl) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  fake_driver_->AllocateRingBuffer(8192);

  EXPECT_FALSE(DropControl(device_));
}

TEST_F(DeviceWarningTest, CannotDropControlTwice) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  fake_driver_->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device_));
  ASSERT_TRUE(DropControl(device_));

  EXPECT_FALSE(DropControl(device_));
}

TEST_F(DeviceWarningTest, CannotControlUnhealthyDevice) {
  fake_driver_->set_health_state(false);
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(HasError(device_));

  EXPECT_FALSE(SetControl(device_));
}

TEST_F(DeviceWarningTest, CannotSetGainWithoutControl) {
  InitializeDeviceForFakeDriver();
  ASSERT_TRUE(InInitializedState(device_));
  RunLoopUntilIdle();
  auto gain_state = DeviceGainState(device_);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());

  constexpr float kNewGainDb = -2.0f;
  EXPECT_FALSE(SetDeviceGain({{
      .muted = true,
      .agc_enabled = true,
      .gain_db = kNewGainDb,
  }}));

  RunLoopUntilIdle();
  gain_state = DeviceGainState(device_);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());
}

// TODO(https://fxbug.dev/42069012): CreateRingBuffer with bad format.

// TODO(https://fxbug.dev/42069012): GetVmo size too large; min_frames too large

}  // namespace media_audio
