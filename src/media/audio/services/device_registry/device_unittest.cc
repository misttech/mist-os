// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/device.h"

#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/zx/clock.h>
#include <zircon/errors.h>

#include <optional>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/common_unittest.h"
#include "src/media/audio/services/device_registry/device_unittest.h"
#include "src/media/audio/services/device_registry/testing/fake_codec.h"
#include "src/media/audio/services/device_registry/testing/fake_composite.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"
#include "src/media/audio/services/device_registry/validate.h"

namespace media_audio {

using ::testing::Optional;

/////////////////////
// Codec tests
//
// Validate that a fake codec is initialized successfully.
TEST_F(CodecTest, Initialization) {
  auto fake_driver = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  EXPECT_TRUE(InInitializedState(device));

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);

  EXPECT_TRUE(device->is_codec());

  EXPECT_TRUE(device->has_codec_properties());
  EXPECT_TRUE(device->checked_for_signalprocessing());
  EXPECT_TRUE(device->has_health_state());
  EXPECT_TRUE(device->dai_format_sets_retrieved());
  EXPECT_TRUE(device->has_plug_state());

  EXPECT_FALSE(device->supports_signalprocessing());
  EXPECT_TRUE(device->info().has_value());
}

TEST_F(CodecTest, InitializationNoDirection) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  EXPECT_TRUE(InInitializedState(device));

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);

  ASSERT_TRUE(device->info().has_value());
  EXPECT_FALSE(device->info()->is_input().has_value());
}

// Validate that a fake codec is initialized to the expected default values.
TEST_F(CodecTest, DeviceInfo) {
  auto fake_driver = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));

  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  ASSERT_EQ(device_presence_watcher()->on_ready_count(), 1u);

  ASSERT_TRUE(device->info().has_value());
  auto info = *device->info();
  EXPECT_TRUE(info.token_id().has_value());

  ASSERT_TRUE(info.device_type().has_value());
  EXPECT_EQ(*info.device_type(), fuchsia_audio_device::DeviceType::kCodec);

  ASSERT_TRUE(info.device_name().has_value());
  EXPECT_TRUE(!info.device_name()->empty());

  ASSERT_TRUE(info.manufacturer().has_value());
  EXPECT_EQ(*info.manufacturer(), FakeCodec::kDefaultManufacturer);

  ASSERT_TRUE(info.product().has_value());
  EXPECT_EQ(*info.product(), FakeCodec::kDefaultProduct);

  ASSERT_TRUE(info.unique_instance_id().has_value());
  EXPECT_EQ(*info.unique_instance_id(), FakeCodec::kDefaultUniqueInstanceId);

  ASSERT_TRUE(info.is_input().has_value());
  EXPECT_FALSE(info.is_input().value());

  EXPECT_FALSE(info.ring_buffer_format_sets().has_value());

  ASSERT_TRUE(info.dai_format_sets().has_value());
  ASSERT_FALSE(info.dai_format_sets()->empty());
  ASSERT_TRUE(info.dai_format_sets()->at(0).format_sets().has_value());
  EXPECT_EQ(*info.dai_format_sets()->at(0).format_sets(), FakeCodec::kDefaultDaiFormatSets);

  EXPECT_FALSE(info.gain_caps().has_value());

  ASSERT_TRUE(info.plug_detect_caps().has_value());
  EXPECT_EQ(*info.plug_detect_caps(), FakeCodec::kDefaultPlugCaps);

  EXPECT_FALSE(info.clock_domain().has_value());

  EXPECT_FALSE(info.signal_processing_elements().has_value());

  EXPECT_FALSE(info.signal_processing_topologies().has_value());
}

// Validate that a driver's dropping the Codec causes a DeviceIsRemoved notification.
TEST_F(CodecTest, Disconnect) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  ASSERT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  ASSERT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  ASSERT_EQ(device_presence_watcher()->on_error_count(), 0u);
  ASSERT_EQ(device_presence_watcher()->on_removal_count(), 0u);

  fake_driver->DropCodec();
  RunLoopUntilIdle();

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_from_ready_count(), 1u);
}

// Validate that a GetHealthState response of an empty struct is considered "healthy".
TEST_F(CodecTest, EmptyHealthResponse) {
  auto fake_driver = MakeFakeCodecOutput();
  fake_driver->set_health_state(std::nullopt);
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  EXPECT_TRUE(InInitializedState(device));

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(CodecTest, Observer) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));

  EXPECT_TRUE(AddObserver(device));
}

TEST_F(CodecTest, Control) {
  auto fake_driver = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  EXPECT_TRUE(DropControl(device));
}

// This tests whether a Codec driver notifies Observers of initial gain state. (It shouldn't.)
TEST_F(CodecTest, InitialGainState) {
  auto fake_driver = MakeFakeCodecInput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  EXPECT_FALSE(notify()->gain_state().has_value())
      << "ObserverNotify was notified of initial GainState";
}

TEST_F(CodecTest, GetDaiFormats) {
  auto fake_driver = MakeFakeCodecInput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_FALSE(IsControlled(device));

  ASSERT_TRUE(AddObserver(device));

  bool received_get_dai_formats_callback = false;
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&received_get_dai_formats_callback, &dai_formats](
          ElementId element_id,
          const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fuchsia_audio_device::kDefaultDaiInterconnectElementId);
        received_get_dai_formats_callback = true;
        for (auto& dai_format_set : formats) {
          dai_formats.push_back(dai_format_set);
        }
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_get_dai_formats_callback);
  EXPECT_EQ(ValidateDaiFormatSets(dai_formats), ZX_OK);
}

// SetDaiFormat is tested (here and in CodecWarningTest) against all states and error cases:
// States: First set, format-change, no-change. ControlNotify::DaiFormatChanged received.
// SetDaiFormat stops Codec; ControlNotify::CodecStopped received if was started.
// Errors: StreamConfig type; Device has error, not controlled; invalid format; unsupported format.
//
// SetDaiFormat - use the first supported format
TEST_F(CodecTest, SetDaiFormat) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));

  ASSERT_TRUE(SetControl(device));
  ASSERT_TRUE(IsControlled(device));

  bool received_get_dai_formats_callback = false;
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&received_get_dai_formats_callback, &dai_formats](
          ElementId element_id,
          const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fuchsia_audio_device::kDefaultDaiInterconnectElementId);
        received_get_dai_formats_callback = true;
        for (auto& dai_format_set : formats) {
          dai_formats.push_back(dai_format_set);
        }
      });
  RunLoopUntilIdle();
  ASSERT_TRUE(received_get_dai_formats_callback);
  ASSERT_FALSE(notify()->dai_format());
  ASSERT_TRUE(device->CodecSetDaiFormat(SafeDaiFormatFromDaiFormatSets(dai_formats)));

  // check that notify received dai_format and codec_format_info
  RunLoopUntilIdle();
  EXPECT_TRUE(notify()->dai_format());
  EXPECT_EQ(ValidateDaiFormat(*notify()->dai_format()), ZX_OK);
  EXPECT_TRUE(notify()->codec_format_info());
  EXPECT_EQ(ValidateCodecFormatInfo(*notify()->codec_format_info()), ZX_OK);
}

// Start and Stop
TEST_F(CodecTest, InitialStop) {
  auto fake_driver = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));

  ASSERT_TRUE(SetControl(device));
  ASSERT_TRUE(IsControlled(device));

  bool received_get_dai_formats_callback = false;
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&received_get_dai_formats_callback, &dai_formats](
          ElementId element_id,
          const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fuchsia_audio_device::kDefaultDaiInterconnectElementId);
        received_get_dai_formats_callback = true;
        for (auto& dai_format_set : formats) {
          dai_formats.push_back(dai_format_set);
        }
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_get_dai_formats_callback);
  ASSERT_TRUE(device->CodecSetDaiFormat(SafeDaiFormatFromDaiFormatSets(dai_formats)));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->dai_format());
  ASSERT_TRUE(notify()->codec_is_stopped());

  // This should do nothing since we are already stopped.
  EXPECT_TRUE(device->CodecStop());

  RunLoopUntilIdle();
  EXPECT_TRUE(notify()->codec_is_stopped());
  // This demonstrates that there was no change as a result of the Stop call.
  EXPECT_EQ(notify()->codec_stop_time()->get(), zx::time::infinite_past().get());
}

TEST_F(CodecTest, Start) {
  auto fake_driver = MakeFakeCodecInput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&dai_formats](ElementId element_id,
                     const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fuchsia_audio_device::kDefaultDaiInterconnectElementId);
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(device->CodecSetDaiFormat(SafeDaiFormatFromDaiFormatSets(dai_formats)));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->dai_format());
  ASSERT_TRUE(notify()->codec_is_stopped());
  auto time_before_start = zx::clock::get_monotonic();

  EXPECT_TRUE(device->CodecStart());

  RunLoopUntilIdle();
  EXPECT_TRUE(notify()->codec_is_started());
  EXPECT_GT(notify()->codec_start_time()->get(), time_before_start.get());
}

TEST_F(CodecTest, SetDaiFormatChange) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&dai_formats](ElementId element_id,
                     const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fuchsia_audio_device::kDefaultDaiInterconnectElementId);
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(device->CodecSetDaiFormat(SafeDaiFormatFromDaiFormatSets(dai_formats)));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->dai_format());
  ASSERT_TRUE(device->CodecStart());

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->codec_is_started());
  auto dai_format2 = SecondDaiFormatFromDaiFormatSets(dai_formats);
  auto time_before_format_change = zx::clock::get_monotonic();

  // Change the DaiFormat
  EXPECT_TRUE(device->CodecSetDaiFormat(dai_format2));

  // Expect codec to be stopped, and format to be changed.
  RunLoopUntilIdle();
  EXPECT_TRUE(notify()->codec_is_stopped());
  EXPECT_GT(notify()->codec_stop_time()->get(), time_before_format_change.get());
  EXPECT_TRUE(notify()->dai_format());
  EXPECT_EQ(*notify()->dai_format(), dai_format2);
  EXPECT_TRUE(notify()->codec_format_info());
}

TEST_F(CodecTest, SetDaiFormatNoChange) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&dai_formats](ElementId element_id,
                     const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fuchsia_audio_device::kDefaultDaiInterconnectElementId);
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  auto safe_format = SafeDaiFormatFromDaiFormatSets(dai_formats);
  ASSERT_TRUE(device->CodecSetDaiFormat(safe_format));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->dai_format());
  ASSERT_TRUE(device->CodecStart());

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->codec_is_started());
  auto time_after_started = zx::clock::get_monotonic();
  // Clear out our notify's DaiFormat so we can detect a new notification of the same DaiFormat.
  notify()->clear_dai_format();

  EXPECT_TRUE(device->CodecSetDaiFormat(safe_format));

  RunLoopUntilIdle();
  // We do not expect this to reset our Start state.
  EXPECT_TRUE(notify()->codec_is_started());
  EXPECT_LT(notify()->codec_start_time()->get(), time_after_started.get());
  // We do not expect to be notified of a format change -- even a "change" to the same format.
  EXPECT_FALSE(notify()->dai_format());
}

TEST_F(CodecTest, StartStop) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&dai_formats](ElementId element_id,
                     const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fuchsia_audio_device::kDefaultDaiInterconnectElementId);
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(device->CodecSetDaiFormat(SafeDaiFormatFromDaiFormatSets(dai_formats)));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->dai_format());
  ASSERT_TRUE(notify()->codec_is_stopped());
  ASSERT_TRUE(device->CodecStart());

  RunLoopUntilIdle();
  auto time_before_stop = zx::clock::get_monotonic();
  ASSERT_TRUE(notify()->codec_is_started());
  ASSERT_TRUE(device->CodecStop());

  RunLoopUntilIdle();
  EXPECT_TRUE(notify()->codec_is_stopped());
  EXPECT_GT(notify()->codec_stop_time()->get(), time_before_stop.get());
}

// Start when already started: no notification; old start_time.
TEST_F(CodecTest, StartStart) {
  auto fake_driver = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&dai_formats](ElementId element_id,
                     const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fuchsia_audio_device::kDefaultDaiInterconnectElementId);
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(device->CodecSetDaiFormat(SafeDaiFormatFromDaiFormatSets(dai_formats)));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->dai_format());
  ASSERT_TRUE(notify()->codec_is_stopped());
  ASSERT_TRUE(device->CodecStart());

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->codec_is_started());
  auto previous_start_time = *notify()->codec_start_time();
  ASSERT_TRUE(device->CodecStart());

  // Should not get a new notification here.
  RunLoopUntilIdle();
  EXPECT_TRUE(notify()->codec_is_started());
  EXPECT_EQ(notify()->codec_start_time()->get(), previous_start_time.get());
}

// Stop when already stopped: no notification; old stop_time.
TEST_F(CodecTest, StopStop) {
  auto fake_driver = MakeFakeCodecInput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&dai_formats](ElementId element_id,
                     const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fuchsia_audio_device::kDefaultDaiInterconnectElementId);
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(device->CodecSetDaiFormat(SafeDaiFormatFromDaiFormatSets(dai_formats)));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->dai_format());
  ASSERT_TRUE(notify()->codec_is_stopped());
  // First Start the Codec so we can explicitly transition into the Stop state.
  ASSERT_TRUE(device->CodecStart());

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->codec_is_started());
  ASSERT_TRUE(device->CodecStop());

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->codec_is_stopped());
  auto previous_stop_time = *notify()->codec_stop_time();

  // Since we are already stopped, this should have no effect.
  EXPECT_TRUE(device->CodecStop());

  RunLoopUntilIdle();
  EXPECT_TRUE(notify()->codec_is_stopped());
  // This demonstrates that there was no change as a result of the Stop call.
  EXPECT_EQ(notify()->codec_stop_time()->get(), previous_stop_time.get());
}

// Reset stops the Codec and resets DaiFormat, Elements and Topology
TEST_F(CodecTest, Reset) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      [&dai_formats](ElementId element_id,
                     const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fuchsia_audio_device::kDefaultDaiInterconnectElementId);
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(device->CodecSetDaiFormat(SafeDaiFormatFromDaiFormatSets(dai_formats)));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->dai_format());
  ASSERT_TRUE(notify()->codec_format_info());
  ASSERT_TRUE(notify()->codec_is_stopped());
  ASSERT_TRUE(device->CodecStart());

  // TODO(https://fxbug.dev/323270827): implement signalprocessing for Codec (topology, gain).
  // When implemented in FakeCodec, set a signalprocessing Topology, and change a signalprocessing
  // Element, and observe that both of these changes are reverted by the call to Reset.

  RunLoopUntilIdle();
  EXPECT_TRUE(notify()->codec_is_started());
  // Verify that the signalprocessing Topology has in fact changed.
  // Verify that the signalprocessing Element has in fact changed.
  auto time_before_reset = zx::clock::get_monotonic();

  EXPECT_TRUE(device->CodecReset());

  RunLoopUntilIdle();
  EXPECT_TRUE(notify()->codec_is_stopped());
  // We were notified that the Codec stopped.
  EXPECT_GT(notify()->codec_stop_time()->get(), time_before_reset.get());
  // We were notified that the Codec reset its DaiFormat (none is now set).
  EXPECT_FALSE(notify()->dai_format());
  EXPECT_FALSE(notify()->codec_format_info());
  // Observe the signalprocessing Elements - were they reset?
  // Observe the signalprocessing Topology - was it reset?
}

// This tests the driver's ability to inform its ObserverNotify of initial plug state.
TEST_F(CodecTest, InitialPlugState) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  EXPECT_TRUE(device_plugged_state(device));
  ASSERT_TRUE(notify()->plug_state());
  EXPECT_EQ(notify()->plug_state()->first, fuchsia_audio_device::PlugState::kPlugged);
  EXPECT_EQ(notify()->plug_state()->second.get(), zx::time::infinite_past().get());
}

// This tests the driver's ability to originate plug changes, such as from jack detection. It also
// validates that plug notifications are delivered through ControlNotify (not just ObserverNotify).
TEST_F(CodecTest, DynamicPlugUpdate) {
  auto fake_driver = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  RunLoopUntilIdle();
  EXPECT_TRUE(device_plugged_state(device));
  ASSERT_TRUE(notify()->plug_state());
  EXPECT_EQ(notify()->plug_state()->first, fuchsia_audio_device::PlugState::kPlugged);
  EXPECT_EQ(notify()->plug_state()->second.get(), zx::time::infinite_past().get());
  notify()->plug_state().reset();

  auto unplug_time = zx::clock::get_monotonic();
  fake_driver->InjectUnpluggedAt(unplug_time);
  RunLoopUntilIdle();
  EXPECT_FALSE(device_plugged_state(device));
  ASSERT_TRUE(notify()->plug_state());
  EXPECT_EQ(notify()->plug_state()->first, fuchsia_audio_device::PlugState::kUnplugged);
  EXPECT_EQ(notify()->plug_state()->second.get(), unplug_time.get());
}

/////////////////////
// Composite tests
//
// Validate that a fake composite is initialized successfully.
TEST_F(CompositeTest, Initialization) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  EXPECT_TRUE(InInitializedState(device));

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);

  EXPECT_TRUE(device->is_composite());

  ASSERT_TRUE(device->info().has_value());
  EXPECT_FALSE(device->info()->is_input().has_value());
}

// Validate that a fake composite is initialized to the expected default values.
TEST_F(CompositeTest, DeviceInfo) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  ASSERT_EQ(device_presence_watcher()->on_ready_count(), 1u);

  ASSERT_TRUE(device->info().has_value());
  auto info = *device->info();

  EXPECT_TRUE(info.token_id().has_value());

  ASSERT_TRUE(info.device_type().has_value());
  EXPECT_EQ(*info.device_type(), fuchsia_audio_device::DeviceType::kComposite);

  ASSERT_TRUE(info.device_name().has_value());
  EXPECT_FALSE(info.device_name()->empty());

  ASSERT_TRUE(info.manufacturer().has_value());
  EXPECT_EQ(*info.manufacturer(), FakeComposite::kDefaultManufacturer);

  ASSERT_TRUE(info.product().has_value());
  EXPECT_EQ(*info.product(), FakeComposite::kDefaultProduct);

  ASSERT_TRUE(info.unique_instance_id().has_value());
  EXPECT_EQ(*info.unique_instance_id(), FakeComposite::kDefaultUniqueInstanceId);

  EXPECT_FALSE(info.is_input().has_value());

  ASSERT_TRUE(info.ring_buffer_format_sets().has_value());
  ASSERT_FALSE(info.ring_buffer_format_sets()->empty());
  ASSERT_EQ(info.ring_buffer_format_sets()->size(), 2u);

  ASSERT_TRUE(info.ring_buffer_format_sets()->at(0).element_id().has_value());
  EXPECT_EQ(*info.ring_buffer_format_sets()->at(0).element_id(), FakeComposite::kSourceRbElementId);
  ASSERT_TRUE(info.ring_buffer_format_sets()->at(0).format_sets().has_value());
  ASSERT_FALSE(info.ring_buffer_format_sets()->at(0).format_sets()->empty());
  EXPECT_EQ(info.ring_buffer_format_sets()->at(0).format_sets()->size(), 1u);
  auto format_set0 = info.ring_buffer_format_sets()->at(0).format_sets()->at(0);
  ASSERT_TRUE(format_set0.frame_rates().has_value());
  EXPECT_THAT(*format_set0.frame_rates(),
              testing::ElementsAreArray(FakeComposite::kDefaultRbFrameRates2));
  ASSERT_TRUE(format_set0.channel_sets().has_value());
  ASSERT_FALSE(format_set0.channel_sets()->empty());
  EXPECT_EQ(format_set0.channel_sets()->size(), 1u);
  ASSERT_TRUE(format_set0.channel_sets()->at(0).attributes().has_value());
  ASSERT_FALSE(format_set0.channel_sets()->at(0).attributes()->empty());
  EXPECT_EQ(format_set0.channel_sets()->at(0).attributes()->size(), 1u);
  ASSERT_FALSE(format_set0.channel_sets()->at(0).attributes()->at(0).max_frequency().has_value());
  ASSERT_TRUE(format_set0.channel_sets()->at(0).attributes()->at(0).min_frequency().has_value());
  EXPECT_EQ(*format_set0.channel_sets()->at(0).attributes()->at(0).min_frequency(),
            FakeComposite::kDefaultRbChannelAttributeMinFrequency2);

  ASSERT_TRUE(info.ring_buffer_format_sets()->at(1).element_id().has_value());
  EXPECT_EQ(*info.ring_buffer_format_sets()->at(1).element_id(), FakeComposite::kDestRbElementId);
  ASSERT_TRUE(info.ring_buffer_format_sets()->at(1).format_sets().has_value());
  ASSERT_FALSE(info.ring_buffer_format_sets()->at(1).format_sets()->empty());
  EXPECT_EQ(info.ring_buffer_format_sets()->at(1).format_sets()->size(), 1u);
  auto format_set1 = info.ring_buffer_format_sets()->at(1).format_sets()->at(0);
  ASSERT_TRUE(format_set1.frame_rates().has_value());
  EXPECT_THAT(*format_set1.frame_rates(),
              testing::ElementsAreArray(FakeComposite::kDefaultRbFrameRates));
  ASSERT_TRUE(format_set1.channel_sets().has_value());
  ASSERT_FALSE(format_set1.channel_sets()->empty());
  EXPECT_EQ(format_set1.channel_sets()->size(), 1u);
  ASSERT_TRUE(format_set1.channel_sets()->at(0).attributes().has_value());
  ASSERT_FALSE(format_set1.channel_sets()->at(0).attributes()->empty());
  EXPECT_EQ(format_set1.channel_sets()->at(0).attributes()->size(), 1u);
  ASSERT_TRUE(format_set1.channel_sets()->at(0).attributes()->at(0).max_frequency().has_value());
  EXPECT_EQ(*format_set1.channel_sets()->at(0).attributes()->at(0).max_frequency(),
            FakeComposite::kDefaultRbChannelAttributeMaxFrequency);
  ASSERT_TRUE(format_set1.channel_sets()->at(0).attributes()->at(0).min_frequency().has_value());
  EXPECT_EQ(*format_set1.channel_sets()->at(0).attributes()->at(0).min_frequency(),
            FakeComposite::kDefaultRbChannelAttributeMinFrequency);

  ASSERT_TRUE(info.dai_format_sets().has_value());
  ASSERT_FALSE(info.dai_format_sets()->empty());
  ASSERT_EQ(info.dai_format_sets()->size(), 2u);

  ASSERT_TRUE(info.dai_format_sets()->at(0).element_id().has_value());
  EXPECT_EQ(*info.dai_format_sets()->at(0).element_id(), FakeComposite::kDestDaiElementId);
  ASSERT_TRUE(info.dai_format_sets()->at(0).format_sets().has_value());
  EXPECT_EQ(info.dai_format_sets()->at(0).format_sets()->size(), 1u);
  EXPECT_THAT(info.dai_format_sets()->at(0).format_sets()->at(0).number_of_channels(),
              testing::ElementsAreArray(FakeComposite::kDefaultDaiNumberOfChannelsSet2));
  EXPECT_THAT(info.dai_format_sets()->at(0).format_sets()->at(0).frame_rates(),
              testing::ElementsAreArray(FakeComposite::kDefaultDaiFrameRates2));
  EXPECT_THAT(info.dai_format_sets()->at(0).format_sets()->at(0).bits_per_slot(),
              testing::ElementsAreArray(FakeComposite::kDefaultDaiBitsPerSlotSet2));
  EXPECT_THAT(info.dai_format_sets()->at(0).format_sets()->at(0).bits_per_sample(),
              testing::ElementsAreArray(FakeComposite::kDefaultDaiBitsPerSampleSet2));
  EXPECT_THAT(info.dai_format_sets()->at(0).format_sets()->at(0).frame_formats(),
              testing::ElementsAreArray(FakeComposite::kDefaultDaiFrameFormatsSet2));
  EXPECT_THAT(info.dai_format_sets()->at(0).format_sets()->at(0).sample_formats(),
              testing::ElementsAreArray(FakeComposite::kDefaultDaiSampleFormatsSet2));

  ASSERT_TRUE(info.dai_format_sets()->at(1).element_id().has_value());
  EXPECT_EQ(*info.dai_format_sets()->at(1).element_id(), FakeComposite::kSourceDaiElementId);
  ASSERT_TRUE(info.dai_format_sets()->at(1).format_sets().has_value());
  EXPECT_EQ(info.dai_format_sets()->at(1).format_sets()->size(), 1u);
  EXPECT_THAT(info.dai_format_sets()->at(1).format_sets()->at(0).number_of_channels(),
              testing::ElementsAreArray(FakeComposite::kDefaultDaiNumberOfChannelsSet));
  EXPECT_THAT(info.dai_format_sets()->at(1).format_sets()->at(0).frame_rates(),
              testing::ElementsAreArray(FakeComposite::kDefaultDaiFrameRates));
  EXPECT_THAT(info.dai_format_sets()->at(1).format_sets()->at(0).bits_per_slot(),
              testing::ElementsAreArray(FakeComposite::kDefaultDaiBitsPerSlotSet));
  EXPECT_THAT(info.dai_format_sets()->at(1).format_sets()->at(0).bits_per_sample(),
              testing::ElementsAreArray(FakeComposite::kDefaultDaiBitsPerSampleSet));
  EXPECT_THAT(info.dai_format_sets()->at(1).format_sets()->at(0).frame_formats(),
              testing::ElementsAreArray(FakeComposite::kDefaultDaiFrameFormatsSet));
  EXPECT_THAT(info.dai_format_sets()->at(1).format_sets()->at(0).sample_formats(),
              testing::ElementsAreArray(FakeComposite::kDefaultDaiSampleFormatsSet));

  EXPECT_FALSE(info.gain_caps().has_value());

  EXPECT_FALSE(info.plug_detect_caps().has_value());

  EXPECT_TRUE(info.clock_domain().has_value());

  ASSERT_TRUE(info.signal_processing_elements().has_value());
  ASSERT_TRUE(info.signal_processing_topologies().has_value());
  EXPECT_FALSE(info.signal_processing_elements()->empty());
  EXPECT_FALSE(info.signal_processing_topologies()->empty());
}

// Validate that a driver's dropping the Composite causes a DeviceIsRemoved notification.
TEST_F(CompositeTest, Disconnect) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  ASSERT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  ASSERT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  ASSERT_EQ(device_presence_watcher()->on_error_count(), 0u);
  ASSERT_EQ(device_presence_watcher()->on_removal_count(), 0u);

  fake_driver->DropComposite();
  RunLoopUntilIdle();

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_from_ready_count(), 1u);
}

// Validate that a GetHealthState response of an empty struct is considered "healthy".
TEST_F(CompositeTest, EmptyHealthResponse) {
  auto fake_driver = MakeFakeComposite();
  fake_driver->set_health_state(std::nullopt);
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  EXPECT_TRUE(InInitializedState(device));

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(CompositeTest, Observer) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(InInitializedState(device));

  EXPECT_TRUE(AddObserver(device));
}

TEST_F(CompositeTest, Control) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  EXPECT_TRUE(DropControl(device));
}

// This tests whether a Composite driver notifies Observers of initial gain state. (It shouldn't.)
TEST_F(CompositeTest, InitialGainState) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  EXPECT_FALSE(notify()->gain_state().has_value())
      << "ObserverNotify was notified of initial GainState";
}

// This tests whether a Composite driver notifies Observers of initial plug state. (It shouldn't.)
TEST_F(CompositeTest, InitialPlugState) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  EXPECT_FALSE(notify()->plug_state().has_value())
      << "ObserverNotify was notified of initial PlugState";
}

// This tests the driver's ability to inform its ObserverNotify of initial signalprocessing state.
TEST_F(CompositeTest, InitialSignalProcessingForObserver) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  EXPECT_TRUE(notify()->topology_id().has_value())
      << "ObserverNotify was not notified of initial TopologyId";
  EXPECT_FALSE(notify()->element_states().empty());

  ASSERT_TRUE(device->info().has_value());
  ASSERT_TRUE(device->info()->signal_processing_elements().has_value());
  EXPECT_FALSE(device->info()->signal_processing_elements()->empty());
  EXPECT_EQ(notify()->element_states().size(),
            device->info()->signal_processing_elements()->size());
}

// This tests the driver's ability to inform its ControlNotify of initial signalprocessing state.
TEST_F(CompositeTest, InitialSignalProcessingForControl) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  RunLoopUntilIdle();
  EXPECT_TRUE(notify()->topology_id().has_value())
      << "ObserverNotify was not notified of initial TopologyId";
  EXPECT_FALSE(notify()->element_states().empty());

  ASSERT_TRUE(device->info().has_value());
  ASSERT_TRUE(device->info()->signal_processing_elements().has_value());
  EXPECT_FALSE(device->info()->signal_processing_elements()->empty());
  EXPECT_EQ(notify()->element_states().size(),
            device->info()->signal_processing_elements()->size());
}

// SetElementState(no-change) should not generate a notification.

// SetTopology(no-change) should not generate a notification.

/////////////////////
// StreamConfig tests
//
// Validate that a fake stream_config with default values is initialized successfully.
TEST_F(StreamConfigTest, Initialization) {
  auto device = InitializeDeviceForFakeStreamConfig(MakeFakeStreamConfigOutput());
  EXPECT_TRUE(InInitializedState(device));

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);

  EXPECT_EQ(device->device_type(), fuchsia_audio_device::DeviceType::kOutput);

  EXPECT_TRUE(device->has_stream_config_properties());
  EXPECT_TRUE(device->checked_for_signalprocessing());
  EXPECT_TRUE(device->has_health_state());
  EXPECT_TRUE(device->ring_buffer_format_sets_retrieved());
  EXPECT_TRUE(device->has_plug_state());
  EXPECT_TRUE(device->has_gain_state());

  EXPECT_FALSE(device->supports_signalprocessing());
  EXPECT_TRUE(device->info().has_value());
}

// Validate that a driver's dropping the StreamConfig causes a DeviceIsRemoved notification.
TEST_F(StreamConfigTest, Disconnect) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  ASSERT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  ASSERT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  ASSERT_EQ(device_presence_watcher()->on_error_count(), 0u);
  ASSERT_EQ(device_presence_watcher()->on_removal_count(), 0u);

  fake_driver->DropStreamConfig();
  RunLoopUntilIdle();

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 0u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_removal_from_ready_count(), 1u);
}

// Validate that a GetHealthState response of an empty struct is considered "healthy".
TEST_F(StreamConfigTest, EmptyHealthResponse) {
  auto fake_driver = MakeFakeStreamConfigInput();
  fake_driver->set_health_state(std::nullopt);
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  EXPECT_TRUE(InInitializedState(device));

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(StreamConfigTest, DistinctTokenIds) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(InInitializedState(device));

  // Set up a second, entirely distinct fake device.
  auto stream_config_endpoints = fidl::CreateEndpoints<fuchsia_hardware_audio::StreamConfig>();
  ASSERT_TRUE(stream_config_endpoints.is_ok());

  auto fake_driver2 = std::make_unique<FakeStreamConfig>(
      stream_config_endpoints->server.TakeChannel(), stream_config_endpoints->client.TakeChannel(),
      dispatcher());
  fake_driver2->set_is_input(true);

  auto device2 = InitializeDeviceForFakeStreamConfig(fake_driver2);
  EXPECT_TRUE(InInitializedState(device2));

  EXPECT_NE(device->token_id(), device2->token_id());

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 2u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(StreamConfigTest, DefaultClock) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(InInitializedState(device));

  EXPECT_EQ(device_clock(device)->domain(), fuchsia_hardware_audio::kClockDomainMonotonic);
  EXPECT_TRUE(device_clock(device)->IdenticalToMonotonicClock());
  EXPECT_FALSE(device_clock(device)->adjustable());

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(StreamConfigTest, ClockInOtherDomain) {
  const uint32_t kNonMonotonicClockDomain = fuchsia_hardware_audio::kClockDomainMonotonic + 1;
  auto fake_driver = MakeFakeStreamConfigInput();
  fake_driver->set_clock_domain(kNonMonotonicClockDomain);
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(InInitializedState(device));

  EXPECT_EQ(device_clock(device)->domain(), kNonMonotonicClockDomain);
  EXPECT_TRUE(device_clock(device)->IdenticalToMonotonicClock());
  EXPECT_TRUE(device_clock(device)->adjustable());

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(StreamConfigTest, DeviceInfo) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
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
  auto fake_driver = MakeFakeStreamConfigOutput();
  fake_driver->set_frame_rates(0, {48000, 48001});
  fake_driver->set_valid_bits_per_sample(0, {12, 15, 20});
  fake_driver->set_bytes_per_sample(0, {2, 4});
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(InInitializedState(device));

  auto valid_bits = ExpectFormatMatch(device, fuchsia_audio::SampleType::kInt16, 2, 48001);
  EXPECT_EQ(valid_bits, 15u);

  valid_bits = ExpectFormatMatch(device, fuchsia_audio::SampleType::kInt32, 2, 48000);
  EXPECT_EQ(valid_bits, 20u);
}

TEST_F(StreamConfigTest, Observer) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(InInitializedState(device));

  EXPECT_TRUE(AddObserver(device));
}

TEST_F(StreamConfigTest, Control) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  EXPECT_TRUE(DropControl(device));
}

// This tests the driver's ability to inform its ObserverNotify of initial gain state.
TEST_F(StreamConfigTest, InitialGainState) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  auto gain_state = device_gain_state(device);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());
  ASSERT_TRUE(notify()->gain_state()) << "ObserverNotify was not notified of initial gain state";
  ASSERT_TRUE(notify()->gain_state()->gain_db());
  EXPECT_EQ(*notify()->gain_state()->gain_db(), 0.0f);
  EXPECT_FALSE(notify()->gain_state()->muted().value_or(false));
  EXPECT_FALSE(notify()->gain_state()->agc_enabled().value_or(false));
}

// This tests the driver's ability to originate gain changes, such as from hardware buttons. It also
// validates that gain notifications are delivered through ControlNotify (not just ObserverNotify).
TEST_F(StreamConfigTest, DynamicGainUpdate) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  RunLoopUntilIdle();
  auto gain_state = device_gain_state(device);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());
  EXPECT_EQ(*notify()->gain_state()->gain_db(), 0.0f);
  EXPECT_FALSE(notify()->gain_state()->muted().value_or(false));
  EXPECT_FALSE(notify()->gain_state()->agc_enabled().value_or(false));
  notify()->gain_state().reset();

  constexpr float kNewGainDb = -2.0f;
  fake_driver->InjectGainChange({{
      .muted = true,
      .agc_enabled = true,
      .gain_db = kNewGainDb,
  }});

  RunLoopUntilIdle();
  gain_state = device_gain_state(device);
  EXPECT_EQ(*gain_state.gain_db(), kNewGainDb);
  EXPECT_TRUE(*gain_state.muted());
  EXPECT_TRUE(*gain_state.agc_enabled());

  ASSERT_TRUE(notify()->gain_state());
  ASSERT_TRUE(notify()->gain_state()->gain_db());
  ASSERT_TRUE(notify()->gain_state()->muted());
  ASSERT_TRUE(notify()->gain_state()->agc_enabled());
  EXPECT_EQ(*notify()->gain_state()->gain_db(), kNewGainDb);
  EXPECT_TRUE(*notify()->gain_state()->muted());
  EXPECT_TRUE(*notify()->gain_state()->agc_enabled());
}

// This tests the ability to set gain to the driver, such as from GUI volume controls.
TEST_F(StreamConfigTest, SetGain) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  RunLoopUntilIdle();
  auto gain_state = device_gain_state(device);
  EXPECT_EQ(*gain_state.gain_db(), 0.0f);
  EXPECT_FALSE(*gain_state.muted());
  EXPECT_FALSE(*gain_state.agc_enabled());
  ASSERT_TRUE(notify()->gain_state()) << "ObserverNotify was not notified of initial gain state";
  EXPECT_EQ(*notify()->gain_state()->gain_db(), 0.0f);
  EXPECT_FALSE(notify()->gain_state()->muted().value_or(false));
  EXPECT_FALSE(notify()->gain_state()->agc_enabled().value_or(false));
  notify()->gain_state().reset();

  constexpr float kNewGainDb = -2.0f;
  EXPECT_TRUE(SetDeviceGain(device, {{
                                        .muted = true,
                                        .agc_enabled = true,
                                        .gain_db = kNewGainDb,
                                    }}));

  RunLoopUntilIdle();
  gain_state = device_gain_state(device);
  EXPECT_EQ(*gain_state.gain_db(), kNewGainDb);
  EXPECT_TRUE(*gain_state.muted());
  EXPECT_TRUE(*gain_state.agc_enabled());

  ASSERT_TRUE(notify()->gain_state() && notify()->gain_state()->gain_db() &&
              notify()->gain_state()->muted() && notify()->gain_state()->agc_enabled());
  EXPECT_EQ(*notify()->gain_state()->gain_db(), kNewGainDb);
  EXPECT_TRUE(notify()->gain_state()->muted().value_or(false));        // Must be present and true.
  EXPECT_TRUE(notify()->gain_state()->agc_enabled().value_or(false));  // Must be present and true.
}

// This tests the driver's ability to inform its ObserverNotify of initial plug state.
TEST_F(StreamConfigTest, InitialPlugState) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  EXPECT_TRUE(device_plugged_state(device));
  ASSERT_TRUE(notify()->plug_state());
  EXPECT_EQ(notify()->plug_state()->first, fuchsia_audio_device::PlugState::kPlugged);
  EXPECT_EQ(notify()->plug_state()->second.get(), zx::time(0).get());
}

TEST_F(StreamConfigTest, DynamicPlugUpdate) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  ASSERT_TRUE(SetControl(device));

  RunLoopUntilIdle();
  EXPECT_TRUE(device_plugged_state(device));
  ASSERT_TRUE(notify()->plug_state());
  EXPECT_EQ(notify()->plug_state()->first, fuchsia_audio_device::PlugState::kPlugged);
  EXPECT_EQ(notify()->plug_state()->second.get(), zx::time(0).get());
  notify()->plug_state().reset();

  auto unplug_time = zx::clock::get_monotonic();
  fake_driver->InjectUnpluggedAt(unplug_time);
  RunLoopUntilIdle();
  EXPECT_FALSE(device_plugged_state(device));
  ASSERT_TRUE(notify()->plug_state());
  EXPECT_EQ(notify()->plug_state()->first, fuchsia_audio_device::PlugState::kUnplugged);
  EXPECT_EQ(notify()->plug_state()->second.get(), unplug_time.get());
}

// Validate that Device can open the driver's RingBuffer FIDL channel.
TEST_F(StreamConfigTest, CreateRingBuffer) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  fake_driver->AllocateRingBuffer(8192);
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
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  fake_driver->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));
  ConnectToRingBufferAndExpectValidClient(device);

  GetRingBufferProperties(device);
}

// TODO(https://fxbug.dev/42069012): Unittest CalculateRequiredRingBufferSizes

TEST_F(StreamConfigTest, RingBufferGetVmo) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  fake_driver->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));
  ConnectToRingBufferAndExpectValidClient(device);

  GetDriverVmoAndExpectValid(device);
}

TEST_F(StreamConfigTest, BasicStartAndStop) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  fake_driver->AllocateRingBuffer(8192);
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
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  fake_driver->AllocateRingBuffer(8192);
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
  ASSERT_TRUE(notify()->delay_info()) << "ControlNotify was not notified of initial delay info";
  ASSERT_TRUE(notify()->delay_info()->internal_delay());
  EXPECT_FALSE(notify()->delay_info()->external_delay());
  EXPECT_EQ(*notify()->delay_info()->internal_delay(), 0);
}

TEST_F(StreamConfigTest, DynamicDelay) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  fake_driver->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto created_ring_buffer = false;
  auto connected_to_ring_buffer_fidl = device->CreateRingBuffer(
      kDefaultRingBufferFormat, 2000,
      [&created_ring_buffer](Device::RingBufferInfo info) { created_ring_buffer = true; });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);
  RunLoopUntilIdle();

  EXPECT_TRUE(created_ring_buffer);
  ASSERT_TRUE(notify()->delay_info()) << "ControlNotify was not notified of initial delay info";
  ASSERT_TRUE(notify()->delay_info()->internal_delay());
  EXPECT_FALSE(notify()->delay_info()->external_delay());
  EXPECT_EQ(*notify()->delay_info()->internal_delay(), 0);
  notify()->delay_info().reset();

  RunLoopUntilIdle();
  EXPECT_FALSE(notify()->delay_info());

  fake_driver->InjectDelayUpdate(zx::nsec(123'456), zx::nsec(654'321));
  RunLoopUntilIdle();
  ASSERT_TRUE(DeviceDelayInfo(device));
  ASSERT_TRUE(DeviceDelayInfo(device)->internal_delay());
  ASSERT_TRUE(DeviceDelayInfo(device)->external_delay());
  EXPECT_EQ(*DeviceDelayInfo(device)->internal_delay(), 123'456);
  EXPECT_EQ(*DeviceDelayInfo(device)->external_delay(), 654'321);

  ASSERT_TRUE(notify()->delay_info()) << "ControlNotify was not notified with updated delay info";
  ASSERT_TRUE(notify()->delay_info()->internal_delay());
  ASSERT_TRUE(notify()->delay_info()->external_delay());
  EXPECT_EQ(*notify()->delay_info()->internal_delay(), 123'456);
  EXPECT_EQ(*notify()->delay_info()->external_delay(), 654'321);
}

TEST_F(StreamConfigTest, ReportsThatItSupportsSetActiveChannels) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);

  ASSERT_TRUE(InInitializedState(device));
  fake_driver->AllocateRingBuffer(8192);
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
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  fake_driver->set_active_channels_supported(false);
  ASSERT_TRUE(InInitializedState(device));
  fake_driver->AllocateRingBuffer(8192);
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
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(InInitializedState(device));
  fake_driver->AllocateRingBuffer(8192);
  fake_driver->set_active_channels_supported(true);
  ASSERT_TRUE(SetControl(device));
  ConnectToRingBufferAndExpectValidClient(device);

  ExpectActiveChannels(device, 0x0003);

  SetActiveChannelsAndExpect(device, 0x0002);
}

// TODO(https://fxbug.dev/42069012): SetActiveChannel no change => no callback (no set_time change).

}  // namespace media_audio
