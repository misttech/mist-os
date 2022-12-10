// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/device_unittest.h"
#include "src/media/audio/services/device_registry/testing/fake_audio_driver.h"

namespace media_audio {

class DeviceWarningTest : public DeviceTestBase {};

// TODO: Test cases for non-compliant drivers? (e.g. min_gain_db > max_gain_db)

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

// TODO: Healthy device Added, then becomes Unhealthy.

TEST_F(DeviceWarningTest, UnhealthyDeviceRemoved) {
  fake_driver_->set_health_state(false);
  device_ = InitializeDeviceForFakeDriver(fake_driver_);
  ASSERT_TRUE(HasError(device_));
  ASSERT_EQ(fake_device_presence_watcher_->ready_devices().size(), 0u);
  ASSERT_EQ(fake_device_presence_watcher_->error_devices().size(), 1u);

  ASSERT_EQ(fake_device_presence_watcher_->on_ready_count(), 0u);
  ASSERT_EQ(fake_device_presence_watcher_->on_error_count(), 1u);
  ASSERT_EQ(fake_device_presence_watcher_->on_removal_count(), 0u);

  RemoveDevice();

  // zx::time deadline = zx::clock::get_monotonic() + kCommandTimeout;
  // while (zx::clock::get_monotonic() < deadline) {
  RunLoopUntilIdle();
  // if (fake_device_presence_watcher_->error_devices().size() == 0u) {
  //   break;
  // }
  // }

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);

  EXPECT_EQ(fake_device_presence_watcher_->on_ready_count(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->on_error_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_from_error_count(), 1u);
}

}  // namespace media_audio
