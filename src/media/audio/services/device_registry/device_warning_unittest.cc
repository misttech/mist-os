// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/common_unittest.h"
#include "src/media/audio/services/device_registry/device.h"
#include "src/media/audio/services/device_registry/device_unittest.h"
#include "src/media/audio/services/device_registry/testing/fake_codec.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"

namespace media_audio {

class CodecWarningTest : public CodecTest {};
class StreamConfigWarningTest : public StreamConfigTest {};

// TODO(https://fxbug.dev/42069012): test non-compliant driver behavior (e.g. min_gain>max_gain).

TEST_F(CodecWarningTest, UnhealthyIsError) {
  auto fake_codec = MakeFakeCodecOutput();
  fake_codec->set_health_state(false);
  auto device = InitializeDeviceForFakeCodec(fake_codec);

  EXPECT_TRUE(HasError(device));
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 1u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);
}

TEST_F(CodecWarningTest, UnhealthyCanBeRemoved) {
  auto fake_codec = MakeFakeCodecInput();
  fake_codec->set_health_state(false);
  auto device = InitializeDeviceForFakeCodec(fake_codec);

  ASSERT_TRUE(HasError(device));
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  ASSERT_EQ(device_presence_watcher()->error_devices().size(), 1u);

  ASSERT_EQ(device_presence_watcher()->on_ready_count(), 0u);
  ASSERT_EQ(device_presence_watcher()->on_error_count(), 1u);
  ASSERT_EQ(device_presence_watcher()->on_removal_count(), 0u);

  fake_codec->DropCodec();
  RunLoopUntilIdle();

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_from_error_count(), 1u);
}

TEST_F(CodecWarningTest, UnhealthyFailsSetControl) {
  auto fake_codec = MakeFakeCodecNoDirection();
  fake_codec->set_health_state(false);
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(HasError(device));

  EXPECT_FALSE(SetControl(device));
}

TEST_F(CodecWarningTest, UnhealthyFailsAddObserver) {
  auto fake_codec = MakeFakeCodecOutput();
  fake_codec->set_health_state(false);
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(HasError(device));

  EXPECT_FALSE(AddObserver(device));
}

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test:
//    * device becomes unhealthy before any Device method. Expect method-specific failure +
//      State::Error notif. Would include Reset, SetDaiFormat, Start, Stop.
//    * device becomes unhealthy after being Observed / Controlled. Expect both to drop.
// For this reason, the only "UnhealthyCodec" tests needed at this time are SetControl/AddObserver.

TEST_F(CodecWarningTest, AlreadyControlledFailsSetControl) {
  auto fake_codec = MakeFakeCodecInput();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));

  EXPECT_TRUE(SetControl(device));

  EXPECT_FALSE(SetControl(device));
  EXPECT_TRUE(IsControlled(device));

  // Even though SetControl failed, the device should still be healthy and configurable.
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);
}

TEST_F(CodecWarningTest, AlreadyObservedFailsAddObserver) {
  auto fake_codec = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(AddObserver(device));

  EXPECT_FALSE(AddObserver(device));

  // Even though AddObserver failed, the device should still be healthy and configurable.
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);
}

TEST_F(CodecWarningTest, CannotDropUnknownCodecControl) {
  auto fake_codec = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));

  EXPECT_FALSE(DropControl(device));

  // Even though DropControl failed, the device should still be healthy and configurable.
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);
}

TEST_F(CodecWarningTest, CannotDropCodecControlTwice) {
  auto fake_codec = MakeFakeCodecInput();
  auto device = InitializeDeviceForFakeCodec(fake_codec);

  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));
  ASSERT_TRUE(DropControl(device));

  EXPECT_FALSE(DropControl(device));

  // Even though DropControl failed, the device should still be healthy and configurable.
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);
}

// GetDaiFormats for FakeCodec that fails the GetDaiFormats call

// GetDaiFormats for FakeCodec that returns bad dai_format_sets

TEST_F(CodecWarningTest, WithoutControlFailsCodecCalls) {
  auto fake_codec = MakeFakeCodecInput();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_FALSE(notify()->dai_format());
  ASSERT_FALSE(notify()->codec_is_started());
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&dai_formats](std::vector<fuchsia_hardware_audio::DaiSupportedFormats> formats) {
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(device->CodecReset());
  EXPECT_FALSE(device->CodecStop());
  EXPECT_FALSE(device->CodecSetDaiFormat(SafeDaiFormatFromDaiSupportedFormats(dai_formats)));
  EXPECT_FALSE(device->CodecStart());

  RunLoopUntilIdle();
  EXPECT_FALSE(notify()->dai_format());
  EXPECT_FALSE(notify()->codec_is_started());
}

// SetDaiFormat with invalid formats: expect a warning.
TEST_F(CodecWarningTest, SetInvalidDaiFormat) {
  auto fake_codec = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  ASSERT_EQ(device_presence_watcher()->error_devices().size(), 0u);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&dai_formats](std::vector<fuchsia_hardware_audio::DaiSupportedFormats> formats) {
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  auto invalid_dai_format = SafeDaiFormatFromDaiSupportedFormats(dai_formats);
  invalid_dai_format.bits_per_sample() = invalid_dai_format.bits_per_slot() + 1;

  EXPECT_FALSE(device->CodecSetDaiFormat(invalid_dai_format));

  RunLoopUntilIdle();
  EXPECT_FALSE(notify()->dai_format());
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(CodecWarningTest, SetUnsupportedDaiFormat) {
  auto fake_codec = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  ASSERT_EQ(device_presence_watcher()->error_devices().size(), 0u);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&dai_formats](std::vector<fuchsia_hardware_audio::DaiSupportedFormats> format_sets) {
        for (const auto &format_set : format_sets) {
          dai_formats.push_back(format_set);
        }
      });

  RunLoopUntilIdle();

  // For this valid, but unsupported format, we expect the call to pass but no notification of a
  // DaiFormat change. The device should remain healthy.
  EXPECT_TRUE(device->CodecSetDaiFormat(UnsupportedDaiFormatFromDaiFormatSets(dai_formats)));

  RunLoopUntilIdle();
  EXPECT_FALSE(notify()->dai_format());
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(CodecWarningTest, StartBeforeSetDaiFormat) {
  auto fake_codec = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  ASSERT_EQ(device_presence_watcher()->error_devices().size(), 0u);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  // We expect this to fail, because the DaiFormat has not yet been set.
  EXPECT_FALSE(device->CodecStart());

  RunLoopUntilIdle();
  EXPECT_FALSE(notify()->codec_is_started());
  // We do expect the device to remain healthy and usable.
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(CodecWarningTest, StopBeforeSetDaiFormat) {
  auto fake_codec = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_codec);
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  ASSERT_EQ(device_presence_watcher()->error_devices().size(), 0u);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  // We expect this to fail, because the DaiFormat has not yet been set.
  EXPECT_FALSE(device->CodecStop());

  RunLoopUntilIdle();
  // We do expect the device to remain healthy and usable.
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

////////////////////
// StreamConfig tests
//
TEST_F(StreamConfigWarningTest, UnhealthyIsError) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  fake_stream_config->set_health_state(false);
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);

  EXPECT_TRUE(HasError(device));
  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 1u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);
}

TEST_F(StreamConfigWarningTest, UnhealthyCanBeRemoved) {
  auto fake_stream_config = MakeFakeStreamConfigInput();
  fake_stream_config->set_health_state(false);
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);

  ASSERT_TRUE(HasError(device));
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  ASSERT_EQ(device_presence_watcher()->error_devices().size(), 1u);

  ASSERT_EQ(device_presence_watcher()->on_ready_count(), 0u);
  ASSERT_EQ(device_presence_watcher()->on_error_count(), 1u);
  ASSERT_EQ(device_presence_watcher()->on_removal_count(), 0u);

  fake_stream_config->DropStreamConfig();
  RunLoopUntilIdle();

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_from_error_count(), 1u);
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

TEST_F(StreamConfigWarningTest, UnhealthyFailsAddObserver) {
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
  auto gain_state = device_gain_state(device);
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
  gain_state = device_gain_state(device);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());
}

TEST_F(StreamConfigWarningTest, CodecDeviceCallsFail) {
  auto fake_stream_config = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_stream_config);

  ASSERT_TRUE(InInitializedState(device));

  EXPECT_FALSE(device->CodecReset());
  EXPECT_FALSE(device->CodecSetDaiFormat(fuchsia_hardware_audio::DaiFormat{{}}));
  EXPECT_FALSE(device->CodecStart());
  EXPECT_FALSE(device->CodecStop());
}

// TODO(https://fxbug.dev/42069012): CreateRingBuffer with bad format.

// TODO(https://fxbug.dev/42069012): GetVmo size too large; min_frames too large

// Validate that Device can reopen the driver's RingBuffer FIDL channel after closing it.
//
// We perform this positive test here because a potential race can produce a WARNING.
// Controls are also Observers, and this test drops and then immediately re-adds a Control.
// Observers are not explicitly dropped; they are weakly held and allowed to self-invalidate,
// which may not occur before it is re-added, causing a WARNING.
TEST_F(StreamConfigWarningTest, CreateRingBufferTwice) {
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
  ASSERT_TRUE(connected_to_ring_buffer_fidl);
  ExpectRingBufferReady(device);
  StartAndExpectValid(device);
  StopAndExpectValid(device);

  device->DropRingBuffer();
  ASSERT_TRUE(device->DropControl());

  ASSERT_TRUE(SetControl(device));
  auto reconnected_to_ring_buffer_fidl =
      device->CreateRingBuffer(kDefaultRingBufferFormat, 2000, [](Device::RingBufferInfo info) {
        EXPECT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  EXPECT_TRUE(reconnected_to_ring_buffer_fidl);
  ExpectRingBufferReady(device);
  StartAndExpectValid(device);
  StopAndExpectValid(device);
}

}  // namespace media_audio
