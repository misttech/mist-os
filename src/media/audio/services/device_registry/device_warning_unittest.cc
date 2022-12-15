// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/device_unittest.h"
#include "src/media/audio/services/device_registry/testing/fake_audio_driver.h"

namespace media_audio {

class DeviceWarningTest : public DeviceTestBase {};

// Test cases for non-compliant drivers? (e.g. min_gain_db > max_gain_db)

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

// TODO(fxbug/dev:117199): When Health can change post-initialization, test: Healthy device becomes
// unhealthy - before any Device method. Expect method-specific failure and State::Error notif.

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

}  // namespace media_audio
