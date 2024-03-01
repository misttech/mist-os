// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/device_unittest.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"

namespace media_audio {

class StreamConfigWarningTest : public DeviceTestBase {};

// TODO(https://fxbug.dev/42069012): test non-compliant driver behavior (e.g. min_gain>max_gain).

TEST_F(StreamConfigWarningTest, UnhealthyIsError) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  fake_stream_config->set_health_state(false);
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);

  EXPECT_TRUE(HasError(device));
  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 1u);

  EXPECT_EQ(fake_device_presence_watcher_->on_ready_count(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->on_error_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_count(), 0u);
}

TEST_F(StreamConfigWarningTest, UnhealthyCanBeRemoved) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  fake_stream_config->set_health_state(false);
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);

  ASSERT_TRUE(HasError(device));
  ASSERT_EQ(fake_device_presence_watcher_->ready_devices().size(), 0u);
  ASSERT_EQ(fake_device_presence_watcher_->error_devices().size(), 1u);

  ASSERT_EQ(fake_device_presence_watcher_->on_ready_count(), 0u);
  ASSERT_EQ(fake_device_presence_watcher_->on_error_count(), 1u);
  ASSERT_EQ(fake_device_presence_watcher_->on_removal_count(), 0u);

  fake_stream_config->DropStreamConfig();
  RunLoopUntilIdle();

  EXPECT_EQ(fake_device_presence_watcher_->ready_devices().size(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->error_devices().size(), 0u);

  EXPECT_EQ(fake_device_presence_watcher_->on_ready_count(), 0u);
  EXPECT_EQ(fake_device_presence_watcher_->on_error_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_count(), 1u);
  EXPECT_EQ(fake_device_presence_watcher_->on_removal_from_error_count(), 1u);
}

TEST_F(StreamConfigWarningTest, AlreadyControlledFailsSetControl) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));

  EXPECT_TRUE(SetControl(device));

  EXPECT_FALSE(SetControl(device));
  EXPECT_TRUE(IsControlled(device));
}

TEST_F(StreamConfigWarningTest, UnhealthyFailsSetControl) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  fake_stream_config->set_health_state(false);
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(HasError(device));

  EXPECT_FALSE(SetControl(device));
}

TEST_F(StreamConfigWarningTest, NoMatchForSupportedRingBufferFormatForClientFormat) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  fake_stream_config->set_frame_rates(0, {48000});
  fake_stream_config->set_valid_bits_per_sample(0, {12, 15, 20});
  fake_stream_config->set_bytes_per_sample(0, {2, 4});
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));

  ExpectNoFormatMatch(device, fuchsia_audio::SampleType::kInt16, 2, 47999);
  ExpectNoFormatMatch(device, fuchsia_audio::SampleType::kInt16, 2, 48001);
  ExpectNoFormatMatch(device, fuchsia_audio::SampleType::kInt16, 1, 48000);
  ExpectNoFormatMatch(device, fuchsia_audio::SampleType::kInt16, 3, 48000);
  ExpectNoFormatMatch(device, fuchsia_audio::SampleType::kUint8, 2, 48000);
  ExpectNoFormatMatch(device, fuchsia_audio::SampleType::kFloat32, 2, 48000);
  ExpectNoFormatMatch(device, fuchsia_audio::SampleType::kFloat64, 2, 48000);
}

TEST_F(StreamConfigWarningTest, CannotAddSameObserverTwice) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(AddObserver(device));

  EXPECT_FALSE(AddObserver(device));
}

TEST_F(StreamConfigWarningTest, UnhealthyAddObserverFails) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  fake_stream_config->set_health_state(false);
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(HasError(device));

  EXPECT_FALSE(AddObserver(device));
}

TEST_F(StreamConfigWarningTest, CannotDropUnknownControl) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));
  fake_stream_config->AllocateRingBuffer(8192);

  EXPECT_FALSE(DropControl(device));
}

TEST_F(StreamConfigWarningTest, CannotDropControlTwice) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);

  ASSERT_TRUE(InInitializedState(device));
  fake_stream_config->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));
  ASSERT_TRUE(DropControl(device));

  EXPECT_FALSE(DropControl(device));
}

TEST_F(StreamConfigWarningTest, WithoutControlFailsSetGain) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);
  ASSERT_TRUE(InInitializedState(device));

  RunLoopUntilIdle();
  auto gain_state = DeviceGainState(device);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());

  constexpr float kNewGainDb = -2.0f;
  EXPECT_FALSE(SetDeviceGain(device, {{
                                         .muted = true,
                                         .agc_enabled = true,
                                         .gain_db = kNewGainDb,
                                     }}));

  RunLoopUntilIdle();
  gain_state = DeviceGainState(device);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());
}

// TODO(https://fxbug.dev/42069012): CreateRingBuffer with bad format.

// TODO(https://fxbug.dev/42069012): GetVmo size too large; min_frames too large

}  // namespace media_audio
