// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/device.h"

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio.signalprocessing/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/zx/clock.h>
#include <zircon/errors.h>

#include <optional>
#include <sstream>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/common_unittest.h"
#include "src/media/audio/services/device_registry/device_unittest.h"
#include "src/media/audio/services/device_registry/testing/fake_codec.h"
#include "src/media/audio/services/device_registry/testing/fake_composite.h"
#include "src/media/audio/services/device_registry/testing/fake_composite_ring_buffer.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"
#include "src/media/audio/services/device_registry/validate.h"

namespace media_audio {

namespace fad = fuchsia_audio_device;

/////////////////////
// Codec tests
//
// Verify that a fake codec is initialized successfully.
TEST_F(CodecTest, Initialization) {
  auto fake_driver = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  EXPECT_TRUE(IsInitialized(device));

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
  EXPECT_TRUE(IsInitialized(device));

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);

  ASSERT_TRUE(device->info().has_value());
  EXPECT_FALSE(device->info()->is_input().has_value());
}

// Verify that a fake codec is initialized to the expected default values.
TEST_F(CodecTest, DeviceInfo) {
  auto fake_driver = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  ASSERT_EQ(device_presence_watcher()->on_ready_count(), 1u);

  ASSERT_TRUE(device->info().has_value());
  auto info = *device->info();
  EXPECT_TRUE(info.token_id().has_value());

  ASSERT_TRUE(info.device_type().has_value());
  EXPECT_EQ(*info.device_type(), fad::DeviceType::kCodec);

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

TEST_F(CodecTest, DistinctTokenIds) {
  auto fake_driver = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  // Set up a second, entirely distinct fake device.
  auto [client, server] = fidl::Endpoints<fuchsia_hardware_audio::Codec>::Create();

  auto fake_driver2 =
      std::make_shared<FakeCodec>(server.TakeChannel(), client.TakeChannel(), dispatcher());
  fake_driver2->set_is_input(true);

  auto device2 = InitializeDeviceForFakeCodec(fake_driver2);
  EXPECT_TRUE(IsInitialized(device2));

  EXPECT_NE(device->token_id(), device2->token_id());

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 2u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

// Verify that a driver's dropping the Codec causes a DeviceIsRemoved notification.
TEST_F(CodecTest, Disconnect) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
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

// Verify that a GetHealthState response of an empty struct is considered "healthy".
TEST_F(CodecTest, EmptyHealthResponse) {
  auto fake_driver = MakeFakeCodecOutput();
  fake_driver->set_health_state(std::nullopt);
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  EXPECT_TRUE(IsInitialized(device));

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(CodecTest, GetClock) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  auto clock = device->GetReadOnlyClock();
  ASSERT_TRUE(clock.is_error());
  EXPECT_EQ(clock.error_value(), ZX_ERR_NOT_SUPPORTED);
}

TEST_F(CodecTest, Observer) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  EXPECT_TRUE(AddObserver(device));
}

TEST_F(CodecTest, Control) {
  auto fake_driver = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  EXPECT_TRUE(DropControl(device));
}

// This tests whether a Codec driver notifies Observers of initial gain state. (It shouldn't.)
TEST_F(CodecTest, InitialGainState) {
  auto fake_driver = MakeFakeCodecInput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  EXPECT_FALSE(notify()->gain_state().has_value())
      << "ObserverNotify was notified of initial GainState";
}

// This tests the driver's ability to inform its ObserverNotify of initial plug state.
TEST_F(CodecTest, InitialPlugState) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  EXPECT_TRUE(device_plugged_state(device));
  ASSERT_TRUE(notify()->plug_state());
  EXPECT_EQ(notify()->plug_state()->first, fad::PlugState::kPlugged);
  EXPECT_EQ(notify()->plug_state()->second.get(), zx::time::infinite_past().get());
}

// This tests the driver's ability to originate plug changes, such as from jack detection. It also
// validates that plug notifications are delivered through ControlNotify (not just ObserverNotify).
TEST_F(CodecTest, DynamicPlugUpdate) {
  auto fake_driver = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  RunLoopUntilIdle();
  EXPECT_TRUE(device_plugged_state(device));
  ASSERT_TRUE(notify()->plug_state());
  EXPECT_EQ(notify()->plug_state()->first, fad::PlugState::kPlugged);
  EXPECT_EQ(notify()->plug_state()->second.get(), zx::time::infinite_past().get());
  notify()->plug_state().reset();

  auto unplug_time = zx::clock::get_monotonic();
  fake_driver->InjectUnpluggedAt(unplug_time);
  RunLoopUntilIdle();
  EXPECT_FALSE(device_plugged_state(device));
  ASSERT_TRUE(notify()->plug_state());
  EXPECT_EQ(notify()->plug_state()->first, fad::PlugState::kUnplugged);
  EXPECT_EQ(notify()->plug_state()->second.get(), unplug_time.get());
}

TEST_F(CodecTest, GetDaiFormats) {
  auto fake_driver = MakeFakeCodecInput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_FALSE(IsControlled(device));

  ASSERT_TRUE(AddObserver(device));

  bool received_get_dai_formats_callback = false;
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      dai_element_id(),
      [&received_get_dai_formats_callback, &dai_formats](
          ElementId element_id,
          const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fad::kDefaultDaiInterconnectElementId);
        received_get_dai_formats_callback = true;
        for (auto& dai_format_set : formats) {
          dai_formats.push_back(dai_format_set);
        }
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_get_dai_formats_callback);
  EXPECT_TRUE(ValidateDaiFormatSets(dai_formats));
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
  ASSERT_TRUE(IsInitialized(device));

  ASSERT_TRUE(SetControl(device));
  ASSERT_TRUE(IsControlled(device));

  bool received_get_dai_formats_callback = false;
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      dai_element_id(),
      [&received_get_dai_formats_callback, &dai_formats](
          ElementId element_id,
          const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fad::kDefaultDaiInterconnectElementId);
        received_get_dai_formats_callback = true;
        for (auto& dai_format_set : formats) {
          dai_formats.push_back(dai_format_set);
        }
      });
  RunLoopUntilIdle();
  ASSERT_TRUE(received_get_dai_formats_callback);
  ASSERT_FALSE(notify()->dai_format());
  device->SetDaiFormat(dai_element_id(), SafeDaiFormatFromDaiFormatSets(dai_formats));

  // check that notify received dai_format and codec_format_info
  RunLoopUntilIdle();
  EXPECT_TRUE(notify()->dai_format());
  EXPECT_TRUE(ValidateDaiFormat(*notify()->dai_format()));
  EXPECT_TRUE(notify()->codec_format_info(dai_element_id()));
  EXPECT_TRUE(ValidateCodecFormatInfo(*notify()->codec_format_info(dai_element_id())));
}

// Start and Stop
TEST_F(CodecTest, InitiallyStopped) {
  auto fake_driver = MakeFakeCodecOutput();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  ASSERT_TRUE(SetControl(device));
  ASSERT_TRUE(IsControlled(device));

  bool received_get_dai_formats_callback = false;
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      dai_element_id(),
      [&received_get_dai_formats_callback, &dai_formats](
          ElementId element_id,
          const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fad::kDefaultDaiInterconnectElementId);
        received_get_dai_formats_callback = true;
        for (auto& dai_format_set : formats) {
          dai_formats.push_back(dai_format_set);
        }
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_get_dai_formats_callback);
  device->SetDaiFormat(dai_element_id(), SafeDaiFormatFromDaiFormatSets(dai_formats));

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
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      dai_element_id(),
      [&dai_formats](ElementId element_id,
                     const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fad::kDefaultDaiInterconnectElementId);
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  device->SetDaiFormat(dai_element_id(), SafeDaiFormatFromDaiFormatSets(dai_formats));

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
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      dai_element_id(),
      [&dai_formats](ElementId element_id,
                     const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fad::kDefaultDaiInterconnectElementId);
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  device->SetDaiFormat(dai_element_id(), SafeDaiFormatFromDaiFormatSets(dai_formats));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->dai_format());
  ASSERT_TRUE(device->CodecStart());

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->codec_is_started());
  auto dai_format2 = SecondDaiFormatFromDaiFormatSets(dai_formats);
  auto time_before_format_change = zx::clock::get_monotonic();

  // Change the DaiFormat
  device->SetDaiFormat(dai_element_id(), dai_format2);

  // Expect codec to be stopped, and format to be changed.
  RunLoopUntilIdle();
  EXPECT_TRUE(notify()->codec_is_stopped());
  EXPECT_GT(notify()->codec_stop_time()->get(), time_before_format_change.get());
  EXPECT_TRUE(notify()->dai_format());
  EXPECT_EQ(*notify()->dai_format(), dai_format2);
  EXPECT_TRUE(notify()->codec_format_info(dai_element_id()));
}

TEST_F(CodecTest, SetDaiFormatNoChange) {
  auto fake_driver = MakeFakeCodecNoDirection();
  auto device = InitializeDeviceForFakeCodec(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      dai_element_id(),
      [&dai_formats](ElementId element_id,
                     const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fad::kDefaultDaiInterconnectElementId);
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  auto safe_format = SafeDaiFormatFromDaiFormatSets(dai_formats);
  device->SetDaiFormat(dai_element_id(), safe_format);

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->dai_format());
  ASSERT_TRUE(device->CodecStart());

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->codec_is_started());
  auto time_after_started = zx::clock::get_monotonic();
  // Clear out our notify's DaiFormat so we can detect a new notification of the same DaiFormat.
  notify()->clear_dai_format(dai_element_id());

  device->SetDaiFormat(dai_element_id(), safe_format);

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
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      dai_element_id(),
      [&dai_formats](ElementId element_id,
                     const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fad::kDefaultDaiInterconnectElementId);
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  device->SetDaiFormat(dai_element_id(), SafeDaiFormatFromDaiFormatSets(dai_formats));

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
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      dai_element_id(),
      [&dai_formats](ElementId element_id,
                     const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fad::kDefaultDaiInterconnectElementId);
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  device->SetDaiFormat(dai_element_id(), SafeDaiFormatFromDaiFormatSets(dai_formats));

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
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      dai_element_id(),
      [&dai_formats](ElementId element_id,
                     const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fad::kDefaultDaiInterconnectElementId);
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  device->SetDaiFormat(dai_element_id(), SafeDaiFormatFromDaiFormatSets(dai_formats));

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
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));
  std::vector<fuchsia_hardware_audio::DaiSupportedFormats> dai_formats;
  device->RetrieveDaiFormatSets(
      dai_element_id(),
      [&dai_formats](ElementId element_id,
                     const std::vector<fuchsia_hardware_audio::DaiSupportedFormats>& formats) {
        EXPECT_EQ(element_id, fad::kDefaultDaiInterconnectElementId);
        dai_formats.push_back(formats[0]);
      });

  RunLoopUntilIdle();
  device->SetDaiFormat(dai_element_id(), SafeDaiFormatFromDaiFormatSets(dai_formats));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->dai_format());
  ASSERT_TRUE(notify()->codec_format_info(dai_element_id()));
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

  EXPECT_TRUE(device->Reset());

  RunLoopUntilIdle();
  EXPECT_TRUE(notify()->codec_is_stopped());
  // We were notified that the Codec stopped.
  EXPECT_GT(notify()->codec_stop_time()->get(), time_before_reset.get());
  // We were notified that the Codec reset its DaiFormat (none is now set).
  EXPECT_FALSE(notify()->dai_format());
  EXPECT_FALSE(notify()->codec_format_info(dai_element_id()));
  // Observe the signalprocessing Elements - were they reset?
  // Observe the signalprocessing Topology - was it reset?
}

/////////////////////
// Composite tests
//
// Verify that a fake composite is initialized successfully.
TEST_F(CompositeTest, Initialization) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  EXPECT_TRUE(IsInitialized(device));

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);

  EXPECT_TRUE(device->is_composite());

  ASSERT_TRUE(device->info().has_value());
  EXPECT_FALSE(device->info()->is_input().has_value());
}

// Verify that a fake composite is initialized to the expected default values.
TEST_F(CompositeTest, DeviceInfo) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  ASSERT_EQ(device_presence_watcher()->on_ready_count(), 1u);

  ASSERT_TRUE(device->info().has_value());
  auto info = *device->info();

  EXPECT_TRUE(info.token_id().has_value());

  ASSERT_TRUE(info.device_type().has_value());
  EXPECT_EQ(*info.device_type(), fad::DeviceType::kComposite);

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

TEST_F(CompositeTest, DistinctTokenIds) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  // Set up a second, entirely distinct fake device.
  auto [client, server] = fidl::Endpoints<fuchsia_hardware_audio::Composite>::Create();

  auto fake_driver2 =
      std::make_shared<FakeComposite>(server.TakeChannel(), client.TakeChannel(), dispatcher());

  auto device2 = InitializeDeviceForFakeComposite(fake_driver2);
  EXPECT_TRUE(IsInitialized(device2));

  EXPECT_NE(device->token_id(), device2->token_id());

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 2u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

// Verify that a driver's dropping the Composite causes a DeviceIsRemoved notification.
TEST_F(CompositeTest, Disconnect) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
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

// Verify that a GetHealthState response of an empty struct is considered "healthy".
TEST_F(CompositeTest, EmptyHealthResponse) {
  auto fake_driver = MakeFakeComposite();
  fake_driver->set_health_state(std::nullopt);
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  EXPECT_TRUE(IsInitialized(device));

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(CompositeTest, DefaultClock) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  EXPECT_EQ(device_clock(device)->domain(), fuchsia_hardware_audio::kClockDomainMonotonic);
  EXPECT_TRUE(device_clock(device)->IdenticalToMonotonicClock());
  EXPECT_FALSE(device_clock(device)->adjustable());

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(CompositeTest, ClockInOtherDomain) {
  const uint32_t kNonMonotonicClockDomain = fuchsia_hardware_audio::kClockDomainMonotonic + 1;
  auto fake_driver = MakeFakeComposite();
  fake_driver->set_clock_domain(kNonMonotonicClockDomain);
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  EXPECT_EQ(device_clock(device)->domain(), kNonMonotonicClockDomain);
  EXPECT_TRUE(device_clock(device)->IdenticalToMonotonicClock());
  EXPECT_TRUE(device_clock(device)->adjustable());

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(CompositeTest, Observer) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  EXPECT_TRUE(AddObserver(device));
}

// This tests the driver's ability to inform its ObserverNotify of initial signalprocessing state.
TEST_F(CompositeTest, InitialSignalProcessingForObserver) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
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

TEST_F(CompositeTest, Control) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  EXPECT_TRUE(DropControl(device));
}

// This tests the driver's ability to inform its ControlNotify of initial signalprocessing state.
TEST_F(CompositeTest, InitialSignalProcessingForControl) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
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

// This tests whether a Composite driver notifies Observers of initial gain state. (It shouldn't.)
TEST_F(CompositeTest, NoInitialGainState) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  EXPECT_FALSE(notify()->gain_state().has_value())
      << "ObserverNotify was notified of initial GainState";
}

// This tests whether a Composite driver notifies Observers of initial plug state. (It shouldn't.)
TEST_F(CompositeTest, NoInitialPlugState) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  EXPECT_FALSE(notify()->plug_state().has_value())
      << "ObserverNotify was notified of initial PlugState";
}

// Verify setting initial DAI format, with ControlNotify receiving appropriate notification.
TEST_F(CompositeTest, SetDaiFormat) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  auto dai_format_sets_by_element = device->dai_format_sets();
  ASSERT_FALSE(dai_format_sets_by_element.empty());
  for (auto dai_element_id : device->dai_endpoint_ids()) {
    notify()->clear_dai_formats();
    auto safe_format =
        SafeDaiFormatFromElementDaiFormatSets(dai_element_id, dai_format_sets_by_element);

    device->SetDaiFormat(dai_element_id, safe_format);

    RunLoopUntilIdle();
    // check that notify received dai_format for this element_id
    EXPECT_TRUE(ExpectDaiFormatMatches(dai_element_id, safe_format));
    EXPECT_TRUE(notify()->codec_format_infos().empty());
    EXPECT_TRUE(notify()->dai_format_errors().empty());
  }
}

// After a DAI format is set, setting it to the SAME format should have no effect.
TEST_F(CompositeTest, SetDaiFormatNoChange) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  auto dai_format_sets_by_element = device->dai_format_sets();
  ASSERT_FALSE(dai_format_sets_by_element.empty());

  for (auto dai_element_id : device->dai_endpoint_ids()) {
    notify()->clear_dai_formats();
    auto safe_format =
        SafeDaiFormatFromElementDaiFormatSets(dai_element_id, dai_format_sets_by_element);
    device->SetDaiFormat(dai_element_id, safe_format);

    RunLoopUntilIdle();
    auto format_match = notify()->dai_formats().find(dai_element_id);
    ASSERT_NE(format_match, notify()->dai_formats().end());
    ASSERT_TRUE(format_match->second.has_value());
    ASSERT_EQ(format_match->second, safe_format);
    ASSERT_TRUE(notify()->codec_format_infos().empty());
    ASSERT_TRUE(notify()->dai_format_errors().empty());
    notify()->clear_dai_format(dai_element_id);

    device->SetDaiFormat(dai_element_id, safe_format);

    RunLoopUntilIdle();
    ASSERT_FALSE(notify()->dai_format_errors().empty());
    auto error_match = notify()->dai_format_errors().find(dai_element_id);
    ASSERT_NE(error_match, notify()->dai_format_errors().end());
    EXPECT_EQ(error_match->second, fad::ControlSetDaiFormatError(0));
    format_match = notify()->dai_formats().find(dai_element_id);
    EXPECT_EQ(format_match, notify()->dai_formats().end());
    EXPECT_TRUE(notify()->codec_format_infos().empty());
  }
}

// After a DAI format is set, CHANGING it should generate the appropriate notifications.
TEST_F(CompositeTest, SetDaiFormatChange) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  auto dai_format_sets_by_element = device->dai_format_sets();
  ASSERT_FALSE(dai_format_sets_by_element.empty());

  for (auto dai_element_id : device->dai_endpoint_ids()) {
    notify()->clear_dai_formats();
    auto safe_format =
        SafeDaiFormatFromElementDaiFormatSets(dai_element_id, dai_format_sets_by_element);
    auto safe_format2 =
        SecondDaiFormatFromElementDaiFormatSets(dai_element_id, dai_format_sets_by_element);
    device->SetDaiFormat(dai_element_id, safe_format);

    RunLoopUntilIdle();
    auto format_match = notify()->dai_formats().find(dai_element_id);
    ASSERT_NE(format_match, notify()->dai_formats().end());
    ASSERT_TRUE(format_match->second.has_value());
    EXPECT_TRUE(ValidateDaiFormat(format_match->second.value()));
    EXPECT_TRUE(notify()->codec_format_infos().empty());
    EXPECT_TRUE(notify()->dai_format_errors().empty());
    notify()->clear_dai_format(dai_element_id);

    device->SetDaiFormat(dai_element_id, safe_format2);

    RunLoopUntilIdle();
    // SetDaiFormat with no-change should cause a DaiElement to emit a not-set notification with 0.
    format_match = notify()->dai_formats().find(dai_element_id);
    EXPECT_TRUE(ExpectDaiFormatMatches(dai_element_id, safe_format2));
    EXPECT_TRUE(notify()->codec_format_infos().empty());
    EXPECT_TRUE(notify()->dai_format_errors().empty());
  }
}

// Reset stops/releases all RingBuffers and resets all Dais / Elements / Topology.
TEST_F(CompositeTest, Reset) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  // Set DAI formats on every DAI endpoint.
  auto dai_format_sets_by_element = device->dai_format_sets();
  // ASSERT_FALSE(dai_format_sets_by_element.empty());
  ASSERT_EQ(device->dai_endpoint_ids().size(), dai_format_sets_by_element.size());
  for (auto dai_element_id : device->dai_endpoint_ids()) {
    auto safe_format =
        SafeDaiFormatFromElementDaiFormatSets(dai_element_id, dai_format_sets_by_element);
    device->SetDaiFormat(dai_element_id, safe_format);

    RunLoopUntilIdle();
    ASSERT_TRUE(ExpectDaiFormatMatches(dai_element_id, safe_format));
  }

  // Create RingBuffers on every RingBuffer endpoint.
  auto ring_buffer_format_sets_by_element = ElementDriverRingBufferFormatSets(device);
  // ASSERT_FALSE(ring_buffer_format_sets_by_element.empty());
  ASSERT_EQ(device->ring_buffer_endpoint_ids().size(), ring_buffer_format_sets_by_element.size());
  for (auto element_id : device->ring_buffer_endpoint_ids()) {
    fake_driver->ReserveRingBufferSize(element_id, 4096);
    auto safe_format = SafeDriverRingBufferFormatFromElementDriverRingBufferFormatSets(
        element_id, ring_buffer_format_sets_by_element);

    std::stringstream stream;
    stream << "Validating CreateRingBuffer on element_id " << element_id << " with format "
           << *safe_format.pcm_format();
    SCOPED_TRACE(stream.str());

    uint32_t requested_ring_buffer_bytes = 2000;
    auto callback_received = false;
    Device::RingBufferInfo rb_info;

    ASSERT_TRUE(device->CreateRingBuffer(
        element_id, safe_format, requested_ring_buffer_bytes,
        [&callback_received](
            const fit::result<fad::ControlCreateRingBufferError, Device::RingBufferInfo>& result) {
          callback_received = true;
          EXPECT_TRUE(result.is_ok());
        }));

    RunLoopUntilIdle();
    ASSERT_TRUE(callback_received);
  }
  EXPECT_EQ(FakeCompositeRingBuffer::count(), device->ring_buffer_endpoint_ids().size());

  EXPECT_TRUE(device->Reset());

  RunLoopUntilIdle();
  // Reset should cause every DaiElement to emit DaiFormatChanged(id, std::nullopt, std::nullopt).
  // So notify()->dai_formats() should contain an entry for each Dai element, of value std::nullopt.
  EXPECT_EQ(notify()->dai_formats().size(), device->dai_endpoint_ids().size());
  for (const auto& [element_id, format] : notify()->dai_formats()) {
    EXPECT_FALSE(format.has_value()) << "{element_id " << element_id << "}";
  }
  EXPECT_TRUE(notify()->codec_format_infos().empty());
  EXPECT_TRUE(notify()->dai_format_errors().empty());

  // Expect any RingBuffers to drop, eventually. Our Control should still be valid though.
  EXPECT_EQ(FakeCompositeRingBuffer::count(), 0u);

  // Expect ElementState notifications, when implemented.

  // Also expect (maybe) a TopologyChanged notification
}

// RingBuffer test cases
//
// Creating a RingBuffer should succeed.
void CompositeTest::TestCreateRingBuffer(const std::shared_ptr<Device>& device,
                                         ElementId element_id,
                                         const fuchsia_hardware_audio::Format& safe_format) {
  std::stringstream stream;
  stream << "Validating CreateRingBuffer on element_id " << element_id << " with format "
         << *safe_format.pcm_format();
  SCOPED_TRACE(stream.str());

  uint32_t requested_ring_buffer_bytes = 4000;
  auto callback_received = false;
  Device::RingBufferInfo rb_info;

  EXPECT_TRUE(device->CreateRingBuffer(
      element_id, safe_format, requested_ring_buffer_bytes,
      [&callback_received,
       &rb_info](fit::result<fad::ControlCreateRingBufferError, Device::RingBufferInfo> result) {
        callback_received = true;
        ASSERT_TRUE(result.is_ok());
        rb_info = std::move(result.value());
      }));

  RunLoopUntilIdle();
  ASSERT_TRUE(callback_received);

  ASSERT_TRUE(rb_info.properties.turn_on_delay().has_value());
  EXPECT_GE(*rb_info.properties.turn_on_delay(), 0);

  ASSERT_TRUE(rb_info.properties.valid_bits_per_sample().has_value());
  EXPECT_LE(*rb_info.properties.valid_bits_per_sample(),
            safe_format.pcm_format()->bytes_per_sample() * 8);

  ASSERT_TRUE(rb_info.ring_buffer.buffer().has_value());
  EXPECT_GT(rb_info.ring_buffer.buffer()->size(), requested_ring_buffer_bytes);
  EXPECT_TRUE(rb_info.ring_buffer.buffer()->vmo().is_valid());

  ASSERT_TRUE(rb_info.ring_buffer.consumer_bytes().has_value());
  EXPECT_GT(*rb_info.ring_buffer.consumer_bytes(), 0u);
  ASSERT_TRUE(rb_info.ring_buffer.producer_bytes().has_value());
  EXPECT_GT(*rb_info.ring_buffer.producer_bytes(), 0u);

  EXPECT_TRUE(*rb_info.ring_buffer.consumer_bytes() >= requested_ring_buffer_bytes ||
              *rb_info.ring_buffer.producer_bytes() >= requested_ring_buffer_bytes);

  ASSERT_TRUE(rb_info.ring_buffer.format()->channel_count().has_value());
  EXPECT_EQ(*rb_info.ring_buffer.format()->channel_count(),
            safe_format.pcm_format()->number_of_channels());

  ASSERT_TRUE(rb_info.ring_buffer.format()->sample_type().has_value());

  ASSERT_TRUE(rb_info.ring_buffer.format()->frames_per_second().has_value());
  EXPECT_EQ(*rb_info.ring_buffer.format()->frames_per_second(),
            safe_format.pcm_format()->frame_rate());

  ASSERT_TRUE(rb_info.ring_buffer.reference_clock().has_value());
  EXPECT_TRUE(rb_info.ring_buffer.reference_clock()->is_valid());

  ASSERT_TRUE(rb_info.ring_buffer.reference_clock_domain().has_value());
  EXPECT_EQ(*rb_info.ring_buffer.reference_clock_domain(),
            fuchsia_hardware_audio::kClockDomainMonotonic);

  // Now client can drop its RingBuffer connection.
  device->DropRingBuffer(element_id);
}

TEST_F(CompositeTest, CreateRingBuffers) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  auto ring_buffer_format_sets_by_element = ElementDriverRingBufferFormatSets(device);
  ASSERT_FALSE(ring_buffer_format_sets_by_element.empty());
  ASSERT_EQ(device->ring_buffer_endpoint_ids().size(), ring_buffer_format_sets_by_element.size());

  for (auto element_id : device->ring_buffer_endpoint_ids()) {
    fake_driver->ReserveRingBufferSize(element_id, 8192);

    auto safe_format = SafeDriverRingBufferFormatFromElementDriverRingBufferFormatSets(
        element_id, ring_buffer_format_sets_by_element);

    TestCreateRingBuffer(device, element_id, safe_format);
  }
}

// Tests on the Composite RingBuffer interface
//
// Maybe, rather than testing the entire CreateRingBuffer meta-method, we should test the individual
// component-level methods: ConnectRingBufferFidl, RetrieveRingBufferProperties, GetVmo.

// Verify that we get DeviceDroppedRingBuffer notifications
TEST_F(CompositeTest, DeviceDroppedRingBuffer) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  auto ring_buffer_format_sets_by_element = ElementDriverRingBufferFormatSets(device);
  ASSERT_FALSE(ring_buffer_format_sets_by_element.empty());
  ASSERT_EQ(device->ring_buffer_endpoint_ids().size(), ring_buffer_format_sets_by_element.size());
  constexpr uint32_t reserved_ring_buffer_bytes = 8192;
  constexpr uint32_t requested_ring_buffer_bytes = 4000;

  for (auto element_id : device->ring_buffer_endpoint_ids()) {
    SCOPED_TRACE(std::string("Testing DropRingBuffer: element_id ") + std::to_string(element_id));
    fake_driver->ReserveRingBufferSize(element_id, reserved_ring_buffer_bytes);
    auto safe_format = SafeDriverRingBufferFormatFromElementDriverRingBufferFormatSets(
        element_id, ring_buffer_format_sets_by_element);
    auto callback_received = false;
    Device::RingBufferInfo rb_info;
    ASSERT_TRUE(device->CreateRingBuffer(
        element_id, safe_format, requested_ring_buffer_bytes,
        [&callback_received](
            const fit::result<fad::ControlCreateRingBufferError, Device::RingBufferInfo>& result) {
          callback_received = true;
          EXPECT_TRUE(result.is_ok());
        }));

    RunLoopUntilIdle();
    ASSERT_TRUE(callback_received);
    ASSERT_TRUE(RingBufferIsStopped(device, element_id));

    fake_driver->DropRingBuffer(element_id);

    RunLoopUntilIdle();
    EXPECT_FALSE(HasRingBuffer(device, element_id));
    EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
    EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
  }
}

// Verify Start and Stop
TEST_F(CompositeTest, RingBufferStartAndStop) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  auto ring_buffer_format_sets_by_element = ElementDriverRingBufferFormatSets(device);
  ASSERT_FALSE(ring_buffer_format_sets_by_element.empty());
  ASSERT_EQ(device->ring_buffer_endpoint_ids().size(), ring_buffer_format_sets_by_element.size());
  constexpr uint32_t reserved_ring_buffer_bytes = 8192;
  constexpr uint32_t requested_ring_buffer_bytes = 4000;

  for (auto element_id : device->ring_buffer_endpoint_ids()) {
    SCOPED_TRACE(std::string("Testing RingBuffer Start/Stop: element_id ") +
                 std::to_string(element_id));
    fake_driver->ReserveRingBufferSize(element_id, reserved_ring_buffer_bytes);
    auto safe_format = SafeDriverRingBufferFormatFromElementDriverRingBufferFormatSets(
        element_id, ring_buffer_format_sets_by_element);
    auto callback_received = false;
    Device::RingBufferInfo rb_info;
    ASSERT_TRUE(device->CreateRingBuffer(
        element_id, safe_format, requested_ring_buffer_bytes,
        [&callback_received](
            const fit::result<fad::ControlCreateRingBufferError, Device::RingBufferInfo>& result) {
          callback_received = true;
          EXPECT_TRUE(result.is_ok());
        }));

    RunLoopUntilIdle();
    ASSERT_TRUE(callback_received);
    zx::time before_start = zx::clock::get_monotonic();
    zx::time start_time;
    callback_received = false;

    device->StartRingBuffer(element_id,
                            [&callback_received, &start_time](zx::result<zx::time> result) {
                              callback_received = true;
                              EXPECT_TRUE(result.is_ok());
                              start_time = result.value();
                              EXPECT_LT(start_time.get(), zx::clock::get_monotonic().get());
                            });

    RunLoopUntilIdle();
    ASSERT_TRUE(callback_received);
    EXPECT_GT(start_time.get(), before_start.get());
    callback_received = false;

    device->StopRingBuffer(element_id, [&callback_received](zx_status_t status) {
      callback_received = true;
      EXPECT_EQ(status, ZX_OK);
    });

    RunLoopUntilIdle();
    EXPECT_TRUE(callback_received);
  }
}

// Verify SetActiveChannels -- for a driver that supports it.
TEST_F(CompositeTest, SetActiveChannelsSupported) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  auto ring_buffer_format_sets_by_element = ElementDriverRingBufferFormatSets(device);
  ASSERT_FALSE(ring_buffer_format_sets_by_element.empty());
  ASSERT_EQ(device->ring_buffer_endpoint_ids().size(), ring_buffer_format_sets_by_element.size());
  constexpr uint32_t reserved_ring_buffer_bytes = 8192;
  constexpr uint32_t requested_ring_buffer_bytes = 4000;

  for (auto element_id : device->ring_buffer_endpoint_ids()) {
    SCOPED_TRACE(std::string("Testing SetActiveChannels (supported): element_id ") +
                 std::to_string(element_id));
    fake_driver->EnableActiveChannelsSupport(element_id);
    fake_driver->ReserveRingBufferSize(element_id, reserved_ring_buffer_bytes);
    auto safe_format = SafeDriverRingBufferFormatFromElementDriverRingBufferFormatSets(
        element_id, ring_buffer_format_sets_by_element);
    auto callback_received = false;
    Device::RingBufferInfo rb_info;
    ASSERT_TRUE(device->CreateRingBuffer(
        element_id, safe_format, requested_ring_buffer_bytes,
        [&callback_received](
            const fit::result<fad::ControlCreateRingBufferError, Device::RingBufferInfo>& result) {
          callback_received = true;
          EXPECT_TRUE(result.is_ok());
        }));

    RunLoopUntilIdle();
    ASSERT_TRUE(callback_received);
    uint64_t channel_bitmask = (1U << safe_format.pcm_format()->number_of_channels()) - 2;
    zx::time set_time;
    callback_received = false;
    zx::time before_set = zx::clock::get_monotonic();

    auto succeeded = device->SetActiveChannels(
        element_id, channel_bitmask, [&callback_received, &set_time](zx::result<zx::time> result) {
          callback_received = true;
          EXPECT_TRUE(result.is_ok());
          set_time = result.value();
          EXPECT_LT(set_time.get(), zx::clock::get_monotonic().get());
        });

    RunLoopUntilIdle();
    ASSERT_TRUE(succeeded);
    ASSERT_TRUE(callback_received);
    EXPECT_GT(set_time.get(), before_set.get());
    // Use the same channel_bitmask value -- it should succeed, but set_time should be unchanged.
    zx::time set_time2;
    callback_received = false;
    zx::time before_set2 = zx::clock::get_monotonic();

    succeeded = device->SetActiveChannels(
        element_id, channel_bitmask, [&callback_received, &set_time2](zx::result<zx::time> result) {
          callback_received = true;
          EXPECT_TRUE(result.is_ok());
          set_time2 = result.value();
        });

    RunLoopUntilIdle();
    EXPECT_TRUE(succeeded);
    EXPECT_TRUE(callback_received);
    EXPECT_LT(set_time2.get(), before_set2.get());
    EXPECT_EQ(set_time.get(), set_time2.get())
        << "set_time should only change if the channel bitmask does (still " << std::hex
        << channel_bitmask << ")";
  }
}

// Verify SetActiveChannels -- for a driver that does not support it.
TEST_F(CompositeTest, SetActiveChannelsUnsupported) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  auto ring_buffer_format_sets_by_element = ElementDriverRingBufferFormatSets(device);
  ASSERT_FALSE(ring_buffer_format_sets_by_element.empty());
  ASSERT_EQ(device->ring_buffer_endpoint_ids().size(), ring_buffer_format_sets_by_element.size());
  constexpr uint32_t reserved_ring_buffer_bytes = 8192;
  constexpr uint32_t requested_ring_buffer_bytes = 4000;

  for (auto element_id : device->ring_buffer_endpoint_ids()) {
    SCOPED_TRACE(std::string("Testing SetActiveChannels (unsupported): element_id ") +
                 std::to_string(element_id));
    fake_driver->DisableActiveChannelsSupport(element_id);
    fake_driver->ReserveRingBufferSize(element_id, reserved_ring_buffer_bytes);
    auto safe_format = SafeDriverRingBufferFormatFromElementDriverRingBufferFormatSets(
        element_id, ring_buffer_format_sets_by_element);
    auto callback_received = false;
    Device::RingBufferInfo rb_info;
    ASSERT_TRUE(device->CreateRingBuffer(
        element_id, safe_format, requested_ring_buffer_bytes,
        [&callback_received](
            const fit::result<fad::ControlCreateRingBufferError, Device::RingBufferInfo>& result) {
          callback_received = true;
          EXPECT_TRUE(result.is_ok());
        }));

    RunLoopUntilIdle();
    ASSERT_TRUE(callback_received);
    uint64_t channel_bitmask = (1U << safe_format.pcm_format()->number_of_channels()) - 2;
    callback_received = false;

    // During CreateRingBuffer, SetActiveChannels is called, so Device already knows if RingBuffer
    // supports this method. In this case it does NOT (DisableActiveChannelsSupport above), so we
    // expect the method to return false (meaning it did NOT call the driver), without no callback.
    auto succeeded = device->SetActiveChannels(
        element_id, channel_bitmask, [&callback_received](zx::result<zx::time> result) {
          callback_received = true;
          FAIL() << "Unexpected response to SetActiveChannels";
        });

    RunLoopUntilIdle();
    EXPECT_FALSE(succeeded);
    EXPECT_FALSE(callback_received);
  }
}

// Verify that we get DelayInfo change notifications
TEST_F(CompositeTest, WatchDelayInfoInitial) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));
  auto ring_buffer_format_sets_by_element = ElementDriverRingBufferFormatSets(device);
  ASSERT_FALSE(ring_buffer_format_sets_by_element.empty());
  ASSERT_EQ(device->ring_buffer_endpoint_ids().size(), ring_buffer_format_sets_by_element.size());
  constexpr uint32_t reserved_ring_buffer_bytes = 8192;
  constexpr uint32_t requested_ring_buffer_bytes = 4000;

  for (auto element_id : device->ring_buffer_endpoint_ids()) {
    SCOPED_TRACE(std::string("Testing RingBuffer initial DelayInfo: element_id ") +
                 std::to_string(element_id));
    fake_driver->ReserveRingBufferSize(element_id, reserved_ring_buffer_bytes);
    auto safe_format = SafeDriverRingBufferFormatFromElementDriverRingBufferFormatSets(
        element_id, ring_buffer_format_sets_by_element);
    auto created_ring_buffer = false;
    Device::RingBufferInfo rb_info;
    ASSERT_TRUE(device->CreateRingBuffer(
        element_id, safe_format, requested_ring_buffer_bytes,
        [&created_ring_buffer](
            const fit::result<fad::ControlCreateRingBufferError, Device::RingBufferInfo>& result) {
          created_ring_buffer = true;
          EXPECT_TRUE(result.is_ok());
        }));

    RunLoopUntilIdle();
    ASSERT_TRUE(created_ring_buffer);
    // Verify that the Device received the expected values from the driver.
    ASSERT_TRUE(DeviceDelayInfo(device, element_id));
    ASSERT_TRUE(DeviceDelayInfo(device, element_id)->internal_delay());
    EXPECT_EQ(*DeviceDelayInfo(device, element_id)->internal_delay(),
              FakeCompositeRingBuffer::kDefaultInternalDelay->get());
    EXPECT_TRUE(!DeviceDelayInfo(device, element_id)->external_delay().has_value() &&
                !FakeCompositeRingBuffer::kDefaultExternalDelay.has_value());

    // Verify that the ControlNotify was sent the expected values.
    ASSERT_TRUE(notify()->delay_info(element_id))
        << "ControlNotify was not notified of initial delay info";
    ASSERT_TRUE(notify()->delay_info(element_id)->internal_delay());
    EXPECT_EQ(*notify()->delay_info(element_id)->internal_delay(),
              FakeCompositeRingBuffer::kDefaultInternalDelay->get());
    EXPECT_TRUE(!notify()->delay_info(element_id)->external_delay().has_value() &&
                !FakeCompositeRingBuffer::kDefaultExternalDelay.has_value());
  }
}

// Dynamic delay updates
TEST_F(CompositeTest, WatchDelayInfoUpdate) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));
  auto ring_buffer_format_sets_by_element = ElementDriverRingBufferFormatSets(device);
  ASSERT_FALSE(ring_buffer_format_sets_by_element.empty());
  ASSERT_EQ(device->ring_buffer_endpoint_ids().size(), ring_buffer_format_sets_by_element.size());
  constexpr uint32_t reserved_ring_buffer_bytes = 8192;
  constexpr uint32_t requested_ring_buffer_bytes = 4000;

  for (auto element_id : device->ring_buffer_endpoint_ids()) {
    SCOPED_TRACE(std::string("Testing RingBuffer initial DelayInfo: element_id ") +
                 std::to_string(element_id));
    fake_driver->ReserveRingBufferSize(element_id, reserved_ring_buffer_bytes);
    auto safe_format = SafeDriverRingBufferFormatFromElementDriverRingBufferFormatSets(
        element_id, ring_buffer_format_sets_by_element);
    auto created_ring_buffer = false;
    Device::RingBufferInfo rb_info;
    ASSERT_TRUE(device->CreateRingBuffer(
        element_id, safe_format, requested_ring_buffer_bytes,
        [&created_ring_buffer](
            const fit::result<fad::ControlCreateRingBufferError, Device::RingBufferInfo>& result) {
          created_ring_buffer = true;
          EXPECT_TRUE(result.is_ok());
        }));

    RunLoopUntilIdle();
    ASSERT_TRUE(created_ring_buffer);
    ASSERT_TRUE(DeviceDelayInfo(device, element_id));
    ASSERT_TRUE(notify()->delay_info(element_id))
        << "ControlNotify was not notified of initial delay info";
  }

  notify()->clear_delay_infos();
  for (auto element_id : device->ring_buffer_endpoint_ids()) {
    EXPECT_FALSE(notify()->delay_info(element_id))
        << "ControlNotify was not cleared for element_id " << element_id;
    // For each element, inject int_delay [`element_id` nsec] and ext_delay [`element_id` usec].
    fake_driver->InjectDelayUpdate(element_id, zx::nsec(static_cast<int64_t>(element_id)),
                                   zx::usec(static_cast<int64_t>(element_id)));
  }

  RunLoopUntilIdle();

  for (auto element_id : device->ring_buffer_endpoint_ids()) {
    // Ensure the Device received the updated values from the driver.
    ASSERT_TRUE(DeviceDelayInfo(device, element_id));
    ASSERT_TRUE(DeviceDelayInfo(device, element_id)->internal_delay());
    ASSERT_TRUE(DeviceDelayInfo(device, element_id)->external_delay());
    EXPECT_EQ(*DeviceDelayInfo(device, element_id)->internal_delay(), ZX_NSEC(element_id));
    EXPECT_EQ(*DeviceDelayInfo(device, element_id)->external_delay(), ZX_USEC(element_id));

    // Ensure that ControlNotify received these updates as well.
    ASSERT_TRUE(notify()->delay_info(element_id))
        << "ControlNotify was not notified with updated delay info";
    ASSERT_TRUE(notify()->delay_info(element_id)->internal_delay());
    ASSERT_TRUE(notify()->delay_info(element_id)->external_delay());
    EXPECT_EQ(*notify()->delay_info(element_id)->internal_delay(), ZX_NSEC(element_id));
    EXPECT_EQ(*notify()->delay_info(element_id)->external_delay(), ZX_USEC(element_id));
  }
}

//// This is needed to support hardware where the audio device is NOT in kClockDomainMonotonic.
//// Until then, it is not a high priority.
// TEST_F(CompositeTest, PositionNotifications) { FAIL() << "NOT YET IMPLEMENTED"; }

// Signalprocessing test cases
//
// Ensure that we captured the full contents of FakeComposite signalprocessing functionality,
// specifically the signalprocessing elements.
TEST_F(CompositeTest, GetElements) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  ASSERT_TRUE(device->info().has_value());
  ASSERT_TRUE(device->info()->signal_processing_elements().has_value());
  auto& elements = device->info()->signal_processing_elements();
  ASSERT_TRUE(elements.has_value());
  ASSERT_EQ(elements->size(), FakeComposite::kElements.size());

  // Get all the `has_value` checks done in automated fashion upfront.
  for (const auto& element : *elements) {
    ASSERT_TRUE(element.id().has_value());
    ASSERT_TRUE(elements->at(0).type().has_value());
    ASSERT_TRUE(element.description().has_value());
    ASSERT_TRUE(element.can_disable().has_value());
    if (element.type() == fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint) {
      ASSERT_TRUE(element.type_specific().has_value());
      ASSERT_TRUE(element.type_specific()->endpoint().has_value());
      ASSERT_TRUE(element.type_specific()->endpoint()->type().has_value());
      ASSERT_TRUE(element.type_specific()->endpoint()->plug_detect_capabilities().has_value());
    }
  }

  EXPECT_EQ(elements->at(0).id(), FakeComposite::kSourceDaiElementId);
  EXPECT_EQ(elements->at(1).id(), FakeComposite::kDestDaiElementId);
  EXPECT_EQ(elements->at(0).type(),
            fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint);
  EXPECT_EQ(elements->at(1).type(),
            fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint);
  EXPECT_FALSE(elements->at(0).can_disable().value());
  EXPECT_FALSE(elements->at(1).can_disable().value());
  EXPECT_EQ(*elements->at(0).description(), *FakeComposite::kSourceDaiElement.description());
  EXPECT_EQ(*elements->at(1).description(), *FakeComposite::kDestDaiElement.description());
  EXPECT_EQ(*elements->at(0).type_specific()->endpoint()->type(),
            fuchsia_hardware_audio_signalprocessing::EndpointType::kDaiInterconnect);
  EXPECT_EQ(*elements->at(1).type_specific()->endpoint()->type(),
            fuchsia_hardware_audio_signalprocessing::EndpointType::kDaiInterconnect);
  EXPECT_EQ(elements->at(0).type_specific()->endpoint()->plug_detect_capabilities(),
            fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kCanAsyncNotify);
  EXPECT_EQ(elements->at(1).type_specific()->endpoint()->plug_detect_capabilities(),
            fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kCanAsyncNotify);

  EXPECT_EQ(elements->at(2).id(), FakeComposite::kSourceRbElementId);
  EXPECT_EQ(elements->at(3).id(), FakeComposite::kDestRbElementId);
  EXPECT_EQ(elements->at(2).type(),
            fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint);
  EXPECT_EQ(elements->at(3).type(),
            fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint);
  EXPECT_FALSE(elements->at(2).can_disable().value());
  EXPECT_FALSE(elements->at(3).can_disable().value());
  EXPECT_EQ(*elements->at(2).description(), *FakeComposite::kSourceRbElement.description());
  EXPECT_EQ(*elements->at(3).description(), *FakeComposite::kDestRbElement.description());
  EXPECT_EQ(*elements->at(2).type_specific()->endpoint()->type(),
            fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer);
  EXPECT_EQ(*elements->at(3).type_specific()->endpoint()->type(),
            fuchsia_hardware_audio_signalprocessing::EndpointType::kRingBuffer);
  EXPECT_EQ(elements->at(2).type_specific()->endpoint()->plug_detect_capabilities(),
            fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kHardwired);
  EXPECT_EQ(elements->at(3).type_specific()->endpoint()->plug_detect_capabilities(),
            fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kHardwired);

  EXPECT_EQ(elements->at(4).id(), FakeComposite::kMuteElementId);
  EXPECT_EQ(elements->at(4).type(), fuchsia_hardware_audio_signalprocessing::ElementType::kMute);
  EXPECT_TRUE(elements->at(4).can_disable().value());
  EXPECT_EQ(*elements->at(4).description(), *FakeComposite::kMuteElement.description());
  EXPECT_FALSE(elements->at(4).type_specific().has_value());
}

// Ensure that we captured the full contents of FakeComposite signalprocessing functionality,
// specifically the signalprocessing topologies.
TEST_F(CompositeTest, GetTopologies) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  ASSERT_TRUE(device->info().has_value());
  ASSERT_TRUE(device->info()->signal_processing_elements().has_value());
  auto& topologies = device->info()->signal_processing_topologies();
  ASSERT_EQ(topologies->size(), FakeComposite::kTopologies.size());

  ASSERT_TRUE(topologies->at(0).id().has_value());
  EXPECT_EQ(*topologies->at(0).id(), FakeComposite::kInputOnlyTopologyId);
  ASSERT_TRUE(topologies->at(0).processing_elements_edge_pairs().has_value());
  EXPECT_EQ(topologies->at(0).processing_elements_edge_pairs()->size(), 1u);
  EXPECT_EQ(topologies->at(0).processing_elements_edge_pairs()->at(0).processing_element_id_from(),
            FakeComposite::kSourceDaiElementId);
  EXPECT_EQ(topologies->at(0).processing_elements_edge_pairs()->at(0).processing_element_id_to(),
            FakeComposite::kDestRbElementId);

  ASSERT_TRUE(topologies->at(1).id().has_value());
  EXPECT_EQ(*topologies->at(1).id(), FakeComposite::kFullDuplexTopologyId);
  ASSERT_TRUE(topologies->at(1).processing_elements_edge_pairs().has_value());
  EXPECT_EQ(topologies->at(1).processing_elements_edge_pairs()->size(), 2u);
  EXPECT_EQ(topologies->at(1).processing_elements_edge_pairs()->at(0).processing_element_id_from(),
            FakeComposite::kSourceDaiElementId);
  EXPECT_EQ(topologies->at(1).processing_elements_edge_pairs()->at(0).processing_element_id_to(),
            FakeComposite::kDestRbElementId);
  EXPECT_EQ(topologies->at(1).processing_elements_edge_pairs()->at(1).processing_element_id_from(),
            FakeComposite::kSourceRbElementId);
  EXPECT_EQ(topologies->at(1).processing_elements_edge_pairs()->at(1).processing_element_id_to(),
            FakeComposite::kDestDaiElementId);

  ASSERT_TRUE(topologies->at(2).id().has_value());
  EXPECT_EQ(*topologies->at(2).id(), FakeComposite::kOutputOnlyTopologyId);
  ASSERT_TRUE(topologies->at(2).processing_elements_edge_pairs().has_value());
  EXPECT_EQ(topologies->at(2).processing_elements_edge_pairs()->size(), 1u);
  EXPECT_EQ(topologies->at(2).processing_elements_edge_pairs()->at(0).processing_element_id_from(),
            FakeComposite::kSourceRbElementId);
  EXPECT_EQ(topologies->at(2).processing_elements_edge_pairs()->at(0).processing_element_id_to(),
            FakeComposite::kDestDaiElementId);
}

// Ensure that we captured the full contents of FakeComposite signalprocessing functionality,
// specifically the initial signalprocessing state.
TEST_F(CompositeTest, WatchElementStateInitial) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(AddObserver(device));

  const auto& states = notify()->element_states();
  ASSERT_EQ(states.size(), FakeComposite::kElements.size());

  auto state = states.find(FakeComposite::kSourceDaiElementId)->second;
  ASSERT_TRUE(state.enabled().has_value());
  ASSERT_TRUE(state.latency().has_value());
  ASSERT_TRUE(state.type_specific().has_value());
  ASSERT_TRUE(state.vendor_specific_data().has_value());

  EXPECT_TRUE(*state.enabled());
  ASSERT_EQ(state.latency()->Which(),
            fuchsia_hardware_audio_signalprocessing::Latency::Tag::kLatencyTime);
  ASSERT_TRUE(state.latency()->latency_time().has_value());
  EXPECT_EQ(state.latency()->latency_time().value(),
            FakeComposite::kSourceDaiElementLatency.latency_time().value());
  ASSERT_EQ(state.type_specific()->Which(),
            fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::Tag::kEndpoint);
  const auto& endpt_state1 = state.type_specific()->endpoint();
  ASSERT_TRUE(endpt_state1.has_value());
  ASSERT_TRUE(endpt_state1->plug_state().has_value());
  ASSERT_TRUE(endpt_state1->plug_state()->plugged().has_value());
  EXPECT_TRUE(*endpt_state1->plug_state()->plugged());
  EXPECT_EQ(*endpt_state1->plug_state()->plug_state_time(), ZX_TIME_INFINITE_PAST);
  ASSERT_EQ(state.vendor_specific_data()->size(), 8u);
  EXPECT_EQ(state.vendor_specific_data()->at(0), 1u);
  EXPECT_EQ(state.vendor_specific_data()->at(7), 8u);

  state = states.find(FakeComposite::kDestDaiElementId)->second;
  ASSERT_TRUE(state.enabled().has_value());
  ASSERT_TRUE(state.latency().has_value());
  ASSERT_TRUE(state.type_specific().has_value());
  ASSERT_TRUE(state.vendor_specific_data().has_value());

  EXPECT_TRUE(*state.enabled());
  ASSERT_EQ(state.latency()->Which(),
            fuchsia_hardware_audio_signalprocessing::Latency::Tag::kLatencyTime);
  ASSERT_TRUE(state.latency()->latency_time().has_value());
  EXPECT_EQ(state.latency()->latency_time().value(),
            FakeComposite::kDestDaiElementLatency.latency_time().value());
  ASSERT_EQ(state.type_specific()->Which(),
            fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::Tag::kEndpoint);
  const auto& endpt_state2 = state.type_specific()->endpoint();
  ASSERT_TRUE(endpt_state2.has_value());
  ASSERT_TRUE(endpt_state2->plug_state().has_value());
  ASSERT_TRUE(endpt_state2->plug_state()->plugged().has_value());
  EXPECT_TRUE(*endpt_state2->plug_state()->plugged());
  EXPECT_EQ(*endpt_state2->plug_state()->plug_state_time(), ZX_TIME_INFINITE_PAST);
  ASSERT_EQ(state.vendor_specific_data()->size(), 9u);
  EXPECT_EQ(state.vendor_specific_data()->at(0), 8u);
  EXPECT_EQ(state.vendor_specific_data()->at(8), 0u);

  state = states.find(FakeComposite::kSourceRbElementId)->second;
  ASSERT_TRUE(state.enabled().has_value());
  ASSERT_TRUE(state.latency().has_value());
  ASSERT_TRUE(state.type_specific().has_value());
  EXPECT_FALSE(state.vendor_specific_data().has_value());

  EXPECT_TRUE(*state.enabled());
  ASSERT_EQ(state.latency()->Which(),
            fuchsia_hardware_audio_signalprocessing::Latency::Tag::kLatencyFrames);
  ASSERT_TRUE(states.find(FakeComposite::kSourceRbElementId)
                  ->second.latency()
                  ->latency_frames()
                  .has_value());
  EXPECT_EQ(state.latency()->latency_frames().value(),
            FakeComposite::kSourceRbElementLatency.latency_frames().value());
  ASSERT_EQ(state.type_specific()->Which(),
            fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::Tag::kEndpoint);
  const auto& endpt_state3 = state.type_specific()->endpoint();
  ASSERT_TRUE(endpt_state3.has_value());
  ASSERT_TRUE(endpt_state3->plug_state().has_value());
  ASSERT_TRUE(endpt_state3->plug_state()->plugged().has_value());
  EXPECT_TRUE(*endpt_state3->plug_state()->plugged());
  EXPECT_EQ(*endpt_state3->plug_state()->plug_state_time(), ZX_TIME_INFINITE_PAST);

  state = states.find(FakeComposite::kDestRbElementId)->second;
  ASSERT_TRUE(state.enabled().has_value());
  ASSERT_TRUE(state.latency().has_value());
  ASSERT_TRUE(state.type_specific().has_value());
  EXPECT_FALSE(state.vendor_specific_data().has_value());

  EXPECT_TRUE(*state.enabled());
  ASSERT_EQ(state.latency()->Which(),
            fuchsia_hardware_audio_signalprocessing::Latency::Tag::kLatencyFrames);
  ASSERT_TRUE(state.latency()->latency_frames().has_value());
  EXPECT_EQ(state.latency()->latency_frames().value(),
            FakeComposite::kDestRbElementLatency.latency_frames().value());
  ASSERT_EQ(state.type_specific()->Which(),
            fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::Tag::kEndpoint);
  const auto& endpt_state4 = state.type_specific()->endpoint();
  ASSERT_TRUE(endpt_state4.has_value());
  ASSERT_TRUE(endpt_state4->plug_state().has_value());
  ASSERT_TRUE(endpt_state4->plug_state()->plugged().has_value());
  EXPECT_TRUE(*endpt_state4->plug_state()->plugged());
  EXPECT_EQ(*endpt_state4->plug_state()->plug_state_time(), ZX_TIME_INFINITE_PAST);

  state = states.find(FakeComposite::kMuteElementId)->second;
  ASSERT_TRUE(state.enabled().has_value());
  EXPECT_FALSE(state.latency().has_value());
  EXPECT_FALSE(state.type_specific().has_value());
  EXPECT_FALSE(state.vendor_specific_data().has_value());

  EXPECT_FALSE(*state.enabled());
}

TEST_F(CompositeTest, WatchElementStateUpdate) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(AddObserver(device));

  auto& elements = *device->info()->signal_processing_elements();
  const auto& states = notify()->element_states();
  ASSERT_EQ(states.size(), FakeComposite::kElements.size());

  // Determine which states we can inject change into.
  std::unordered_map<ElementId, fuchsia_hardware_audio_signalprocessing::ElementState>
      element_states_to_inject;
  auto plug_change_time_to_inject = zx::clock::get_monotonic();
  for (const auto& element : elements) {
    auto element_id = *element.id();
    auto match_state = states.find(element_id);
    ASSERT_NE(match_state, states.end());
    auto state = match_state->second;

    // Handle the Mute node
    if (element.type() == fuchsia_hardware_audio_signalprocessing::ElementType::kMute &&
        element.can_disable().value_or(false)) {
      // By configuration, our Mute starts disabled (we enable it as our ElementState change).
      ASSERT_TRUE(state.enabled().has_value());
      EXPECT_FALSE(*state.enabled());
      element_states_to_inject.insert_or_assign(
          element_id, fuchsia_hardware_audio_signalprocessing::ElementState{{.enabled = true}});
      continue;
    }

    // Then weed out any non-pluggable elements.
    if (element.type() != fuchsia_hardware_audio_signalprocessing::ElementType::kEndpoint ||
        !element.type_specific().has_value() || !element.type_specific()->endpoint().has_value() ||
        element.type_specific()->endpoint()->plug_detect_capabilities() !=
            fuchsia_hardware_audio_signalprocessing::PlugDetectCapabilities::kCanAsyncNotify) {
      continue;
    }
    if (!state.type_specific().has_value() || !state.type_specific()->endpoint().has_value() ||
        !state.type_specific()->endpoint()->plug_state().has_value() ||
        !state.type_specific()->endpoint()->plug_state()->plugged().has_value() ||
        !state.type_specific()->endpoint()->plug_state()->plug_state_time().has_value()) {
      continue;
    }
    auto was_plugged = state.type_specific()->endpoint()->plug_state()->plugged();
    auto new_state = fuchsia_hardware_audio_signalprocessing::ElementState{{
        .type_specific =
            fuchsia_hardware_audio_signalprocessing::TypeSpecificElementState::WithEndpoint(
                fuchsia_hardware_audio_signalprocessing::EndpointElementState{{
                    fuchsia_hardware_audio_signalprocessing::PlugState{{
                        !was_plugged,
                        plug_change_time_to_inject.get(),
                    }},
                }}),
        .enabled = true,
        .latency =
            fuchsia_hardware_audio_signalprocessing::Latency::WithLatencyTime(ZX_USEC(element_id)),
        .vendor_specific_data = {{'0', '1', '2', '3', '4', '5', '6', '7', '8', '9', 'A', 'B', 'C',
                                  'D', 'E', 'F', 'Z'}},  // 'Z' is located at byte [16].
    }};
    ASSERT_EQ(new_state.vendor_specific_data()->size(), 17u) << "Test configuration error";
    element_states_to_inject.insert_or_assign(element_id, new_state);
  }

  if (element_states_to_inject.empty()) {
    GTEST_SKIP()
        << "No element states can be changed, so dynamic element_state change cannot be tested";
  }

  notify()->clear_element_states();

  // Inject the changes.
  for (const auto& [element_id, element_state] : element_states_to_inject) {
    fake_driver->InjectElementStateChange(element_id, element_state);
  }

  RunLoopUntilIdle();
  EXPECT_EQ(element_states_to_inject.size(), notify()->element_states().size());
  for (const auto& [element_id, state_received] : notify()->element_states()) {
    // Compare to actual static values we know.
    if (element_id == FakeComposite::kMuteElementId) {
      EXPECT_FALSE(state_received.type_specific().has_value());
      ASSERT_TRUE(state_received.enabled().has_value());
      EXPECT_FALSE(state_received.latency().has_value());
      EXPECT_FALSE(state_received.vendor_specific_data().has_value());

      EXPECT_EQ(state_received.enabled(), true);
    } else {
      ASSERT_TRUE(state_received.type_specific().has_value());
      ASSERT_TRUE(state_received.type_specific()->endpoint().has_value());
      ASSERT_TRUE(state_received.type_specific()->endpoint()->plug_state().has_value());
      ASSERT_TRUE(state_received.type_specific()->endpoint()->plug_state()->plugged().has_value());
      ASSERT_TRUE(
          state_received.type_specific()->endpoint()->plug_state()->plug_state_time().has_value());
      EXPECT_EQ(*state_received.type_specific()->endpoint()->plug_state()->plug_state_time(),
                plug_change_time_to_inject.get());

      ASSERT_TRUE(state_received.enabled().has_value());
      EXPECT_EQ(state_received.enabled(), true);

      ASSERT_TRUE(state_received.latency().has_value());
      ASSERT_EQ(state_received.latency()->Which(),
                fuchsia_hardware_audio_signalprocessing::Latency::Tag::kLatencyTime);
      EXPECT_EQ(state_received.latency()->latency_time().value(), ZX_USEC(element_id));

      ASSERT_TRUE(state_received.vendor_specific_data().has_value());
      ASSERT_EQ(state_received.vendor_specific_data()->size(), 17u);
      EXPECT_EQ(state_received.vendor_specific_data()->at(16), 'Z');
    }

    // Compare to what we injected.
    ASSERT_FALSE(element_states_to_inject.find(element_id) == element_states_to_inject.end())
        << "WatchElementState response received for unknown element_id " << element_id;
    const auto& state_injected = element_states_to_inject.find(element_id)->second;
    EXPECT_EQ(state_received, state_injected);
  }
}

TEST_F(CompositeTest, WatchTopologyInitial) {
  auto fake_driver = MakeFakeComposite();
  fake_driver->InjectTopologyChange(FakeComposite::kFullDuplexTopologyId);
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->topology_id().has_value());
  EXPECT_EQ(*notify()->topology_id(), FakeComposite::kFullDuplexTopologyId);
}

TEST_F(CompositeTest, WatchTopologyUpdate) {
  auto fake_driver = MakeFakeComposite();
  fake_driver->InjectTopologyChange(FakeComposite::kFullDuplexTopologyId);
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->topology_id().has_value());
  EXPECT_EQ(*notify()->topology_id(), FakeComposite::kFullDuplexTopologyId);
  notify()->clear_topology_id();

  RunLoopUntilIdle();
  EXPECT_FALSE(notify()->topology_id().has_value());

  fake_driver->InjectTopologyChange(FakeComposite::kInputOnlyTopologyId);

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->topology_id().has_value());
  EXPECT_EQ(*notify()->topology_id(), FakeComposite::kInputOnlyTopologyId);
}

TEST_F(CompositeTest, SetTopology) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->topology_id().has_value());
  TopologyId current_topology_id = *notify()->topology_id();

  TopologyId topology_id_to_set = 0;
  if (device->topology_ids().size() < 2) {
    GTEST_SKIP() << "Not enough topologies to run this test case";
  }
  for (auto t : device->topology_ids()) {
    if (t != current_topology_id) {
      topology_id_to_set = t;
      break;
    }
  }

  EXPECT_EQ(device->SetTopology(topology_id_to_set), ZX_OK);

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->topology_id().has_value());
  EXPECT_EQ(*notify()->topology_id(), topology_id_to_set);
}

TEST_F(CompositeTest, SetElementState) {
  auto fake_driver = MakeFakeComposite();
  auto device = InitializeDeviceForFakeComposite(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  RunLoopUntilIdle();
  ASSERT_TRUE(notify()->element_states().find(FakeComposite::kMuteElementId) !=
              notify()->element_states().end());
  notify()->clear_element_states();
  fuchsia_hardware_audio_signalprocessing::ElementState state{{.enabled = true}};

  EXPECT_EQ(device->SetElementState(FakeComposite::kMuteElementId, state), ZX_OK);

  RunLoopUntilIdle();
  ASSERT_FALSE(notify()->element_states().find(FakeComposite::kMuteElementId) ==
               notify()->element_states().end());
  auto new_state = notify()->element_states().find(FakeComposite::kMuteElementId)->second;
  ASSERT_TRUE(new_state.enabled().has_value());
  EXPECT_EQ(*new_state.enabled(), true);
  EXPECT_FALSE(new_state.latency().has_value());
  EXPECT_FALSE(new_state.type_specific().has_value());
  EXPECT_FALSE(new_state.vendor_specific_data().has_value());
}

/////////////////////
// StreamConfig tests
//
// Verify that a fake stream_config with default values is initialized successfully.
TEST_F(StreamConfigTest, Initialization) {
  auto device = InitializeDeviceForFakeStreamConfig(MakeFakeStreamConfigOutput());
  EXPECT_TRUE(IsInitialized(device));

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);

  EXPECT_EQ(device_presence_watcher()->on_ready_count(), 1u);
  EXPECT_EQ(device_presence_watcher()->on_error_count(), 0u);
  EXPECT_EQ(device_presence_watcher()->on_removal_count(), 0u);

  EXPECT_EQ(device->device_type(), fad::DeviceType::kOutput);

  EXPECT_TRUE(device->has_stream_config_properties());
  EXPECT_TRUE(device->checked_for_signalprocessing());
  EXPECT_TRUE(device->has_health_state());
  EXPECT_TRUE(device->ring_buffer_format_sets_retrieved());
  EXPECT_TRUE(device->has_plug_state());
  EXPECT_TRUE(device->has_gain_state());

  EXPECT_FALSE(device->supports_signalprocessing());
  EXPECT_TRUE(device->info().has_value());
}

TEST_F(StreamConfigTest, DeviceInfo) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  auto info = GetDeviceInfo(device);

  EXPECT_TRUE(info.token_id());
  EXPECT_TRUE(info.device_type());
  EXPECT_EQ(*info.device_type(), fad::DeviceType::kOutput);
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

TEST_F(StreamConfigTest, DistinctTokenIds) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  // Set up a second, entirely distinct fake device.
  auto [client, server] = fidl::Endpoints<fuchsia_hardware_audio::StreamConfig>::Create();

  auto fake_driver2 =
      std::make_shared<FakeStreamConfig>(server.TakeChannel(), client.TakeChannel(), dispatcher());
  fake_driver2->set_is_input(true);

  auto device2 = InitializeDeviceForFakeStreamConfig(fake_driver2);
  EXPECT_TRUE(IsInitialized(device2));

  EXPECT_NE(device->token_id(), device2->token_id());

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 2u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

// Verify that a driver's dropping the StreamConfig causes a DeviceIsRemoved notification.
TEST_F(StreamConfigTest, Disconnect) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
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

// Verify that a GetHealthState response of an empty struct is considered "healthy".
TEST_F(StreamConfigTest, EmptyHealthResponse) {
  auto fake_driver = MakeFakeStreamConfigInput();
  fake_driver->set_health_state(std::nullopt);
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  EXPECT_TRUE(IsInitialized(device));

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(StreamConfigTest, DefaultClock) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

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
  ASSERT_TRUE(IsInitialized(device));

  EXPECT_EQ(device_clock(device)->domain(), kNonMonotonicClockDomain);
  EXPECT_TRUE(device_clock(device)->IdenticalToMonotonicClock());
  EXPECT_TRUE(device_clock(device)->adjustable());

  EXPECT_EQ(device_presence_watcher()->ready_devices().size(), 1u);
  EXPECT_EQ(device_presence_watcher()->error_devices().size(), 0u);
}

TEST_F(StreamConfigTest, SupportedDriverFormatForClientFormat) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  fake_driver->set_frame_rates(0, {48000, 48001});
  fake_driver->set_valid_bits_per_sample(0, {12, 15, 20});
  fake_driver->set_bytes_per_sample(0, {2, 4});
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  auto valid_bits = ExpectFormatMatch(device, ring_buffer_element_id(),
                                      fuchsia_audio::SampleType::kInt16, 2, 48001);
  EXPECT_EQ(valid_bits, 15u);

  valid_bits = ExpectFormatMatch(device, ring_buffer_element_id(),
                                 fuchsia_audio::SampleType::kInt32, 2, 48000);
  EXPECT_EQ(valid_bits, 20u);
}

TEST_F(StreamConfigTest, Observer) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));

  EXPECT_TRUE(AddObserver(device));
}

TEST_F(StreamConfigTest, Control) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  EXPECT_TRUE(DropControl(device));
}

// This tests the driver's ability to inform its ObserverNotify of initial gain state.
TEST_F(StreamConfigTest, InitialGainState) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
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

// This tests the driver's ability to originate gain changes, such as from hardware buttons. It
// also validates that gain notifications are delivered through ControlNotify (not just
// ObserverNotify).
TEST_F(StreamConfigTest, DynamicGainUpdate) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
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
  ASSERT_TRUE(IsInitialized(device));
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
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(AddObserver(device));

  RunLoopUntilIdle();
  EXPECT_TRUE(device_plugged_state(device));
  ASSERT_TRUE(notify()->plug_state());
  EXPECT_EQ(notify()->plug_state()->first, fad::PlugState::kPlugged);
  EXPECT_EQ(notify()->plug_state()->second.get(), zx::time(0).get());
}

TEST_F(StreamConfigTest, DynamicPlugUpdate) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  ASSERT_TRUE(SetControl(device));

  RunLoopUntilIdle();
  EXPECT_TRUE(device_plugged_state(device));
  ASSERT_TRUE(notify()->plug_state());
  EXPECT_EQ(notify()->plug_state()->first, fad::PlugState::kPlugged);
  EXPECT_EQ(notify()->plug_state()->second.get(), zx::time(0).get());
  notify()->plug_state().reset();

  auto unplug_time = zx::clock::get_monotonic();
  fake_driver->InjectUnpluggedAt(unplug_time);
  RunLoopUntilIdle();
  EXPECT_FALSE(device_plugged_state(device));
  ASSERT_TRUE(notify()->plug_state());
  EXPECT_EQ(notify()->plug_state()->first, fad::PlugState::kUnplugged);
  EXPECT_EQ(notify()->plug_state()->second.get(), unplug_time.get());
}

// Verify that Device can open the driver's RingBuffer FIDL channel.
TEST_F(StreamConfigTest, CreateRingBuffer) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  fake_driver->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto connected_to_ring_buffer_fidl = device->CreateRingBuffer(
      ring_buffer_element_id(), kDefaultRingBufferFormat, 2000,
      [](const fit::result<fad::ControlCreateRingBufferError, Device::RingBufferInfo>& result) {
        ASSERT_TRUE(result.is_ok()) << fidl::ToUnderlying(result.error_value());
        auto& info = result.value();
        ASSERT_TRUE(info.ring_buffer.buffer());
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
  ASSERT_TRUE(IsInitialized(device));
  fake_driver->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));
  ConnectToRingBufferAndExpectValidClient(device, ring_buffer_element_id());

  GetRingBufferProperties(device, ring_buffer_element_id());
}

// TODO(https://fxbug.dev/42069012): Unittest CalculateRequiredRingBufferSizes

TEST_F(StreamConfigTest, RingBufferGetVmo) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  fake_driver->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));
  ConnectToRingBufferAndExpectValidClient(device, ring_buffer_element_id());

  GetDriverVmoAndExpectValid(device, ring_buffer_element_id());
}

TEST_F(StreamConfigTest, BasicStartAndStop) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  fake_driver->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto connected_to_ring_buffer_fidl = device->CreateRingBuffer(
      ring_buffer_element_id(), kDefaultRingBufferFormat, 2000,
      [](const fit::result<fad::ControlCreateRingBufferError, Device::RingBufferInfo>& result) {
        ASSERT_TRUE(result.is_ok());
        auto& info = result.value();
        ASSERT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);

  ExpectRingBufferReady(device, ring_buffer_element_id());

  StartAndExpectValid(device, ring_buffer_element_id());
  StopAndExpectValid(device, ring_buffer_element_id());
}

TEST_F(StreamConfigTest, WatchDelayInfoInitial) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  fake_driver->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto created_ring_buffer = false;
  auto connected_to_ring_buffer_fidl = device->CreateRingBuffer(
      ring_buffer_element_id(), kDefaultRingBufferFormat, 2000,
      [&created_ring_buffer](
          const fit::result<fad::ControlCreateRingBufferError, Device::RingBufferInfo>& result) {
        EXPECT_TRUE(result.is_ok());
        created_ring_buffer = true;
      });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);
  RunLoopUntilIdle();

  // Verify that the Device received the expected values from the driver.
  EXPECT_TRUE(created_ring_buffer);
  ASSERT_TRUE(DeviceDelayInfo(device, ring_buffer_element_id()));
  ASSERT_TRUE(DeviceDelayInfo(device, ring_buffer_element_id())->internal_delay());
  EXPECT_FALSE(DeviceDelayInfo(device, ring_buffer_element_id())->external_delay());
  EXPECT_EQ(*DeviceDelayInfo(device, ring_buffer_element_id())->internal_delay(), 0);

  // Verify that the ControlNotify was sent the expected values.
  ASSERT_TRUE(notify()->delay_info()) << "ControlNotify was not notified of initial delay info";
  ASSERT_TRUE(notify()->delay_info()->internal_delay());
  EXPECT_FALSE(notify()->delay_info()->external_delay());
  EXPECT_EQ(*notify()->delay_info()->internal_delay(), 0);
}

TEST_F(StreamConfigTest, WatchDelayInfoUpdate) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  fake_driver->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));
  auto created_ring_buffer = false;
  auto connected_to_ring_buffer_fidl = device->CreateRingBuffer(
      ring_buffer_element_id(), kDefaultRingBufferFormat, 2000,
      [&created_ring_buffer](
          const fit::result<fad::ControlCreateRingBufferError, Device::RingBufferInfo>& result) {
        EXPECT_TRUE(result.is_ok());
        created_ring_buffer = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(connected_to_ring_buffer_fidl);
  EXPECT_TRUE(created_ring_buffer);
  ASSERT_TRUE(notify()->delay_info()) << "ControlNotify was not notified of initial delay info";
  ASSERT_TRUE(notify()->delay_info()->internal_delay());
  EXPECT_FALSE(notify()->delay_info()->external_delay());
  EXPECT_EQ(*notify()->delay_info()->internal_delay(), 0);
  notify()->clear_delay_info();

  RunLoopUntilIdle();
  EXPECT_FALSE(notify()->delay_info());
  fake_driver->InjectDelayUpdate(zx::nsec(123'456), zx::nsec(654'321));

  RunLoopUntilIdle();
  ASSERT_TRUE(DeviceDelayInfo(device, ring_buffer_element_id()));
  ASSERT_TRUE(DeviceDelayInfo(device, ring_buffer_element_id())->internal_delay());
  ASSERT_TRUE(DeviceDelayInfo(device, ring_buffer_element_id())->external_delay());
  EXPECT_EQ(*DeviceDelayInfo(device, ring_buffer_element_id())->internal_delay(), 123'456);
  EXPECT_EQ(*DeviceDelayInfo(device, ring_buffer_element_id())->external_delay(), 654'321);

  ASSERT_TRUE(notify()->delay_info()) << "ControlNotify was not notified with updated delay info";
  ASSERT_TRUE(notify()->delay_info()->internal_delay());
  ASSERT_TRUE(notify()->delay_info()->external_delay());
  EXPECT_EQ(*notify()->delay_info()->internal_delay(), 123'456);
  EXPECT_EQ(*notify()->delay_info()->external_delay(), 654'321);
}

TEST_F(StreamConfigTest, ReportsThatItSupportsSetActiveChannels) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);

  ASSERT_TRUE(IsInitialized(device));
  fake_driver->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto connected_to_ring_buffer_fidl = device->CreateRingBuffer(
      ring_buffer_element_id(), kDefaultRingBufferFormat, 2000,
      [](const fit::result<fad::ControlCreateRingBufferError, Device::RingBufferInfo>& result) {
        ASSERT_TRUE(result.is_ok());
        auto& info = result.value();
        ASSERT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);

  ExpectRingBufferReady(device, ring_buffer_element_id());
  EXPECT_TRUE(device->supports_set_active_channels(ring_buffer_element_id()).value_or(false));
}

TEST_F(StreamConfigTest, ReportsThatItDoesNotSupportSetActiveChannels) {
  auto fake_driver = MakeFakeStreamConfigOutput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  fake_driver->set_active_channels_supported(false);
  ASSERT_TRUE(IsInitialized(device));
  fake_driver->AllocateRingBuffer(8192);
  ASSERT_TRUE(SetControl(device));

  auto connected_to_ring_buffer_fidl = device->CreateRingBuffer(
      ring_buffer_element_id(), kDefaultRingBufferFormat, 2000,
      [](const fit::result<fad::ControlCreateRingBufferError, Device::RingBufferInfo>& result) {
        ASSERT_TRUE(result.is_ok());
        auto& info = result.value();
        ASSERT_TRUE(info.ring_buffer.buffer());
        EXPECT_GT(info.ring_buffer.buffer()->size(), 2000u);

        EXPECT_TRUE(info.ring_buffer.format());
        EXPECT_TRUE(info.ring_buffer.producer_bytes());
        EXPECT_TRUE(info.ring_buffer.consumer_bytes());
        EXPECT_TRUE(info.ring_buffer.reference_clock());
      });
  EXPECT_TRUE(connected_to_ring_buffer_fidl);

  ExpectRingBufferReady(device, ring_buffer_element_id());
  EXPECT_FALSE(device->supports_set_active_channels(ring_buffer_element_id()).value_or(true));
}

TEST_F(StreamConfigTest, SetActiveChannels) {
  auto fake_driver = MakeFakeStreamConfigInput();
  auto device = InitializeDeviceForFakeStreamConfig(fake_driver);
  ASSERT_TRUE(IsInitialized(device));
  fake_driver->AllocateRingBuffer(8192);
  fake_driver->set_active_channels_supported(true);
  ASSERT_TRUE(SetControl(device));
  ConnectToRingBufferAndExpectValidClient(device, ring_buffer_element_id());

  ExpectActiveChannels(device, ring_buffer_element_id(), 0x0003);

  SetInitialActiveChannelsAndExpect(device, ring_buffer_element_id(), 0x0002);
}

// TODO(https://fxbug.dev/42069012): SetActiveChannel no change => no callback (no set_time change).

}  // namespace media_audio
