// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
#include <lib/fidl/cpp/enum.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>
#include <zircon/errors.h>

#include <string>

#include <gtest/gtest.h>

#include "src/media/audio/services/common/testing/test_server_and_async_client.h"
#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/common_unittest.h"
#include "src/media/audio/services/device_registry/control_server.h"
#include "src/media/audio/services/device_registry/testing/fake_codec.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"

namespace media_audio {
namespace {

using Control = fuchsia_audio_device::Control;
using DriverClient = fuchsia_audio_device::DriverClient;

class ControlServerWarningTest : public AudioDeviceRegistryServerTestBase,
                                 public fidl::AsyncEventHandler<fuchsia_audio_device::Control>,
                                 public fidl::AsyncEventHandler<fuchsia_audio_device::RingBuffer> {
 protected:
  std::optional<TokenId> WaitForAddedDeviceTokenId(
      fidl::Client<fuchsia_audio_device::Registry>& registry_client) {
    std::optional<TokenId> added_device_id;
    registry_client->WatchDevicesAdded().Then(
        [&added_device_id](
            fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
          ASSERT_TRUE(result.is_ok()) << result.error_value();
          ASSERT_TRUE(result->devices());
          ASSERT_EQ(result->devices()->size(), 1u);
          ASSERT_TRUE(result->devices()->at(0).token_id());
          added_device_id = *result->devices()->at(0).token_id();
        });
    RunLoopUntilIdle();
    return added_device_id;
  }

  // Obtain a control via ControlCreator/Create (not the synthetic CreateTestControlServer method).
  fidl::Client<fuchsia_audio_device::Control> ConnectToControl(
      fidl::Client<fuchsia_audio_device::ControlCreator>& control_creator_client,
      TokenId token_id) {
    auto [control_client_end, control_server_end] =
        CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Control>();
    auto control_client = fidl::Client<fuchsia_audio_device::Control>(
        std::move(control_client_end), dispatcher(), control_fidl_handler_.get());
    bool received_callback = false;
    control_creator_client
        ->Create({{
            .token_id = token_id,
            .control_server =
                fidl::ServerEnd<fuchsia_audio_device::Control>(std::move(control_server_end)),
        }})
        .Then([&received_callback](
                  fidl::Result<fuchsia_audio_device::ControlCreator::Create>& result) {
          ASSERT_TRUE(result.is_ok()) << result.error_value();
          received_callback = true;
        });
    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);
    EXPECT_TRUE(control_client.is_valid());
    return control_client;
  }
};

class ControlServerCodecWarningTest : public ControlServerWarningTest {
 protected:
  std::unique_ptr<FakeCodec> CreateAndEnableDriverWithDefaults() {
    auto fake_driver = CreateFakeCodecInput();

    adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test codec name",
                                           fuchsia_audio_device::DeviceType::kCodec,
                                           DriverClient::WithCodec(fake_driver->Enable())));
    RunLoopUntilIdle();
    return fake_driver;
  }
};

class ControlServerStreamConfigWarningTest : public ControlServerWarningTest {
 protected:
  std::unique_ptr<FakeStreamConfig> CreateAndEnableDriverWithDefaults() {
    auto fake_driver = CreateFakeStreamConfigOutput();

    adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test output name",
                                           fuchsia_audio_device::DeviceType::kOutput,
                                           DriverClient::WithStreamConfig(fake_driver->Enable())));
    RunLoopUntilIdle();
    return fake_driver;
  }

  // Device error causes ControlNotify->DeviceHasError
  // Driver drops StreamConfig causes ControlNotify->DeviceIsRemoved
  // Client closes RingBuffer does NOT cause ControlNotify->DeviceIsRemoved or DeviceHasError
  // Driver drops RingBuffer does NOT cause ControlNotify->DeviceIsRemoved or DeviceHasError

  void TestSetGainBadState(const std::optional<fuchsia_audio_device::GainState>& bad_state,
                           fuchsia_audio_device::ControlSetGainError expected_error) {
    auto fake_driver = CreateFakeStreamConfigOutput();

    fake_driver->set_can_mute(false);
    fake_driver->set_can_agc(false);
    fake_driver->AllocateRingBuffer(8192);
    adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test output name",
                                           fuchsia_audio_device::DeviceType::kOutput,
                                           DriverClient::WithStreamConfig(fake_driver->Enable())));
    RunLoopUntilIdle();

    auto registry = CreateTestRegistryServer();
    auto added_id = WaitForAddedDeviceTokenId(registry->client());
    auto control_creator = CreateTestControlCreatorServer();
    auto control_client = ConnectToControl(control_creator->client(), *added_id);
    RunLoopUntilIdle();

    ASSERT_EQ(ControlServer::count(), 1u);
    auto [ring_buffer_client_end, ring_buffer_server_end] =
        CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
    auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
        std::move(ring_buffer_client_end), dispatcher(), ring_buffer_fidl_handler_.get());
    bool received_callback = false;

    control_client
        ->SetGain({{
            .target_state = bad_state,
        }})
        .Then([&received_callback, expected_error](fidl::Result<Control::SetGain>& result) {
          ASSERT_TRUE(result.is_error());
          ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
          EXPECT_EQ(result.error_value().domain_error(), expected_error) << result.error_value();
          received_callback = true;
        });

    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);
    EXPECT_EQ(ControlServer::count(), 1u);
  }

  void TestCreateRingBufferBadOptions(
      const std::optional<fuchsia_audio_device::RingBufferOptions>& bad_options,
      fuchsia_audio_device::ControlCreateRingBufferError expected_error) {
    auto fake_driver = CreateAndEnableDriverWithDefaults();
    fake_driver->AllocateRingBuffer(8192);
    auto registry = CreateTestRegistryServer();
    auto added_id = WaitForAddedDeviceTokenId(registry->client());
    auto control_creator = CreateTestControlCreatorServer();
    auto control_client = ConnectToControl(control_creator->client(), *added_id);
    RunLoopUntilIdle();

    ASSERT_EQ(ControlServer::count(), 1u);
    auto [ring_buffer_client_end, ring_buffer_server_end] =
        CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
    auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
        std::move(ring_buffer_client_end), dispatcher(), ring_buffer_fidl_handler_.get());
    bool received_callback = false;

    control_client
        ->CreateRingBuffer({{
            .options = bad_options,
            .ring_buffer_server = fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(
                std::move(ring_buffer_server_end)),
        }})
        .Then([&received_callback,
               expected_error](fidl::Result<Control::CreateRingBuffer>& result) {
          ASSERT_TRUE(result.is_error());
          ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
          EXPECT_EQ(result.error_value().domain_error(), expected_error) << result.error_value();
          received_callback = true;
        });

    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);
    EXPECT_EQ(ControlServer::count(), 1u);
  }
};

/////////////////////
// Codec tests

// SetDaiFormat when already pending
TEST_F(ControlServerCodecWarningTest, SetDaiFormatWhenAlreadyPending) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service_->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto dai_format = SafeDaiFormatFromDaiSupportedFormats(device->dai_format_sets());
  auto dai_format2 = SecondDaiFormatFromDaiSupportedFormats(device->dai_format_sets());
  auto received_callback = false;
  auto received_callback2 = false;

  control->client()
      ->SetDaiFormat({{.dai_format = dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });
  control->client()
      ->SetDaiFormat({{.dai_format = dai_format}})
      .Then([&received_callback2](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback2 = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlSetDaiFormatError::kAlreadyPending)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback && received_callback2);
  EXPECT_EQ(ControlServer::count(), 1u);
}

// SetDaiFormat invalid
TEST_F(ControlServerCodecWarningTest, SetDaiFormatInvalidFormat) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service_->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto invalid_dai_format = SafeDaiFormatFromDaiSupportedFormats(device->dai_format_sets());
  invalid_dai_format.bits_per_sample() = invalid_dai_format.bits_per_slot() + 1;
  auto received_callback = false;

  control->client()
      ->SetDaiFormat({{.dai_format = invalid_dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlSetDaiFormatError::kInvalidDaiFormat)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

// SetDaiFormat unsupported
TEST_F(ControlServerCodecWarningTest, SetDaiFormatUnsupportedFormat) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service_->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto unsupported_dai_format = UnsupportedDaiFormatFromDaiFormatSets(device->dai_format_sets());
  auto received_callback = false;

  control->client()
      ->SetDaiFormat({{.dai_format = unsupported_dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlSetDaiFormatError::kFormatMismatch)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

// Start when already pending
TEST_F(ControlServerCodecWarningTest, CodecStartWhenAlreadyPending) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service_->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto dai_format = SafeDaiFormatFromDaiSupportedFormats(device->dai_format_sets());
  auto received_callback = false;
  control->client()
      ->SetDaiFormat({{.dai_format = dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  received_callback = false;
  auto received_callback2 = false;

  control->client()->CodecStart().Then(
      [&received_callback](fidl::Result<Control::CodecStart>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });
  control->client()->CodecStart().Then(
      [&received_callback2](fidl::Result<Control::CodecStart>& result) {
        received_callback2 = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCodecStartError::kAlreadyPending)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback && received_callback2);
  EXPECT_EQ(ControlServer::count(), 1u);
}

// Start before SetDaiFormat
TEST_F(ControlServerCodecWarningTest, CodecStartBeforeSetDaiFormat) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service_->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()->CodecStart().Then(
      [&received_callback](fidl::Result<Control::CodecStart>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCodecStartError::kDaiFormatNotSet)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

// Start when Started
TEST_F(ControlServerCodecWarningTest, CodecStartWhenStarted) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service_->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto dai_format = SafeDaiFormatFromDaiSupportedFormats(device->dai_format_sets());
  auto received_callback = false;
  control->client()
      ->SetDaiFormat({{.dai_format = dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  received_callback = false;
  control->client()->CodecStart().Then(
      [&received_callback](fidl::Result<Control::CodecStart>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  received_callback = false;

  control->client()->CodecStart().Then(
      [&received_callback](fidl::Result<Control::CodecStart>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCodecStartError::kAlreadyStarted)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

// Stop when already pending
TEST_F(ControlServerCodecWarningTest, CodecStopWhenAlreadyPending) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service_->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto dai_format = SafeDaiFormatFromDaiSupportedFormats(device->dai_format_sets());
  auto received_callback = false;
  control->client()
      ->SetDaiFormat({{.dai_format = dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  received_callback = false;
  control->client()->CodecStart().Then(
      [&received_callback](fidl::Result<Control::CodecStart>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  received_callback = false;
  auto received_callback2 = false;

  control->client()->CodecStop().Then(
      [&received_callback](fidl::Result<Control::CodecStop>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });
  control->client()->CodecStop().Then(
      [&received_callback2](fidl::Result<Control::CodecStop>& result) {
        received_callback2 = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCodecStopError::kAlreadyPending)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback && received_callback2);
  EXPECT_EQ(ControlServer::count(), 1u);
}

// Stop before SetDaiFormat
TEST_F(ControlServerCodecWarningTest, CodecStopBeforeSetDaiFormat) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service_->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()->CodecStop().Then(
      [&received_callback](fidl::Result<Control::CodecStop>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCodecStopError::kDaiFormatNotSet)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

// Stop when Stopped
TEST_F(ControlServerCodecWarningTest, CodecStopWhenStopped) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service_->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto dai_format = SafeDaiFormatFromDaiSupportedFormats(device->dai_format_sets());
  auto received_callback = false;
  control->client()
      ->SetDaiFormat({{.dai_format = dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  received_callback = false;

  control->client()->CodecStop().Then(
      [&received_callback](fidl::Result<Control::CodecStop>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCodecStopError::kAlreadyStopped)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

// SetGain WRONG_DEVICE_TYPE
TEST_F(ControlServerCodecWarningTest, SetGainFails) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service_->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()
      ->SetGain({{
          .target_state = fuchsia_audio_device::GainState{{
              .gain_db = 0.0f,
              .muted = false,
              .agc_enabled = false,
          }},
      }})
      .Then([&received_callback](fidl::Result<Control::SetGain>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlSetGainError::kWrongDeviceType)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

// CreateRingBuffer WRONG_DEVICE_TYPE
TEST_F(ControlServerCodecWarningTest, CreateRingBufferFails) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service_->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();

  auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
      std::move(ring_buffer_client_end), dispatcher(), ring_buffer_fidl_handler_.get());

  control->client()
      ->CreateRingBuffer({{
          .options = fuchsia_audio_device::RingBufferOptions{{
              .format = fuchsia_audio::Format{{
                  .sample_type = fuchsia_audio::SampleType::kInt16,
                  .channel_count = 2,
                  .frames_per_second = 48000,
              }},
              .ring_buffer_min_bytes = 2000,
          }},
          .ring_buffer_server =
              fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreateRingBufferError::kWrongDeviceType)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

// Add negative test cases for SetTopology and SetElementState (once implemented)
//
// TODO(https://fxbug.dev/323270827): implement signalprocessing for Codec (topology, gain).

/////////////////////
// StreamConfig tests
TEST_F(ControlServerStreamConfigWarningTest, SetGainMissingState) {
  TestSetGainBadState(std::nullopt, fuchsia_audio_device::ControlSetGainError::kInvalidGainState);
}

TEST_F(ControlServerStreamConfigWarningTest, SetGainEmptyState) {
  TestSetGainBadState(fuchsia_audio_device::GainState(),
                      fuchsia_audio_device::ControlSetGainError::kInvalidGainDb);
}

TEST_F(ControlServerStreamConfigWarningTest, SetGainMissingGainDb) {
  TestSetGainBadState(fuchsia_audio_device::GainState{{
                          .muted = false,
                          .agc_enabled = false,
                      }},
                      fuchsia_audio_device::ControlSetGainError::kInvalidGainDb);
}

TEST_F(ControlServerStreamConfigWarningTest, SetGainTooHigh) {
  TestSetGainBadState(fuchsia_audio_device::GainState{{
                          .gain_db = +200.0f,
                          .muted = false,
                          .agc_enabled = false,
                      }},
                      fuchsia_audio_device::ControlSetGainError::kGainOutOfRange);
}

TEST_F(ControlServerStreamConfigWarningTest, SetGainTooLow) {
  TestSetGainBadState(fuchsia_audio_device::GainState{{
                          .gain_db = -200.0f,
                          .muted = false,
                          .agc_enabled = false,
                      }},
                      fuchsia_audio_device::ControlSetGainError::kGainOutOfRange);
}

TEST_F(ControlServerStreamConfigWarningTest, SetGainBadMute) {
  TestSetGainBadState(fuchsia_audio_device::GainState{{
                          .gain_db = 0.0f,
                          .muted = true,
                          .agc_enabled = false,
                      }},
                      fuchsia_audio_device::ControlSetGainError::kMuteUnavailable);
}

TEST_F(ControlServerStreamConfigWarningTest, SetGainBadAgc) {
  TestSetGainBadState(fuchsia_audio_device::GainState{{
                          .gain_db = 0.0f,
                          .muted = false,
                          .agc_enabled = true,
                      }},
                      fuchsia_audio_device::ControlSetGainError::kAgcUnavailable);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferMissingOptions) {
  TestCreateRingBufferBadOptions(
      std::nullopt, fuchsia_audio_device::ControlCreateRingBufferError::kInvalidOptions);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferEmptyOptions) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions(),
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferMissingFormat) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = std::nullopt,
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferEmptyFormat) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format(),
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferMissingSampleType) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .channel_count = 2,
              .frames_per_second = 48000,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferBadSampleType) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kFloat64,
              .channel_count = 2,
              .frames_per_second = 48000,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kFormatMismatch);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferMissingChannelCount) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .frames_per_second = 48000,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferBadChannelCount) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .channel_count = 7,
              .frames_per_second = 48000,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kFormatMismatch);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferMissingFramesPerSecond) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .channel_count = 2,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidFormat);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferBadFramesPerSecond) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .channel_count = 2,
              .frames_per_second = 97531,
          }},
          .ring_buffer_min_bytes = 8192,
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kFormatMismatch);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferMissingRingBufferMinBytes) {
  TestCreateRingBufferBadOptions(
      fuchsia_audio_device::RingBufferOptions{{
          .format = fuchsia_audio::Format{{
              .sample_type = fuchsia_audio::SampleType::kInt16,
              .channel_count = 2,
              .frames_per_second = 48000,
          }},
      }},
      fuchsia_audio_device::ControlCreateRingBufferError::kInvalidMinBytes);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferWhilePending) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();
  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();

  ASSERT_EQ(ControlServer::count(), 1u);
  auto [ring_buffer_client_end1, ring_buffer_server_end1] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  auto [ring_buffer_client_end2, ring_buffer_server_end2] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();

  bool received_callback_1 = false, received_callback_2 = false;
  auto options = fuchsia_audio_device::RingBufferOptions{{
      .format = fuchsia_audio::Format{{
          .sample_type = fuchsia_audio::SampleType::kInt16,
          .channel_count = 2,
          .frames_per_second = 48000,
      }},
      .ring_buffer_min_bytes = 8192,
  }};
  control_client
      ->CreateRingBuffer({{
          .options = options,
          .ring_buffer_server =
              fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_server_end1)),
      }})
      .Then([&received_callback_1](fidl::Result<Control::CreateRingBuffer>& result) {
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        received_callback_1 = true;
      });
  control_client
      ->CreateRingBuffer({{
          .options = options,
          .ring_buffer_server =
              fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_server_end2)),
      }})
      .Then([&received_callback_2](fidl::Result<Control::CreateRingBuffer>& result) {
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreateRingBufferError::kAlreadyPending)
            << result.error_value();
        received_callback_2 = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback_1 && received_callback_2);
  EXPECT_EQ(ControlServer::count(), 1u);
  EXPECT_TRUE(control_client.is_valid());
}

// TODO(https://fxbug.dev/42069012): Enable this unittest to test the upper limit of VMO size
// (4Gb).
//   This is not high-priority since even at the service's highest supported bitrate (192kHz,
//   8-channel, float64), a 4Gb ring-buffer would be 5.8 minutes long!
TEST_F(ControlServerStreamConfigWarningTest, DISABLED_CreateRingBufferHugeRingBufferMinBytes) {
  auto fake_driver = CreateFakeStreamConfigOutput();

  fake_driver->clear_formats();
  std::vector<fuchsia::hardware::audio::ChannelAttributes> channel_vector;
  channel_vector.push_back(fuchsia::hardware::audio::ChannelAttributes());
  fake_driver->set_channel_sets(0, 0, std::move(channel_vector));
  fake_driver->set_sample_formats(0, {fuchsia::hardware::audio::SampleFormat::PCM_UNSIGNED});
  fake_driver->set_bytes_per_sample(0, {1});
  fake_driver->set_valid_bits_per_sample(0, {8});
  fake_driver->set_frame_rates(0, {48000});

  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test output name",
                                         fuchsia_audio_device::DeviceType::kOutput,
                                         DriverClient::WithStreamConfig(fake_driver->Enable())));
  fake_driver->AllocateRingBuffer(8192);
  RunLoopUntilIdle();

  auto registry = CreateTestRegistryServer();
  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();

  ASSERT_EQ(ControlServer::count(), 1u);
  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
      std::move(ring_buffer_client_end), dispatcher(), ring_buffer_fidl_handler_.get());
  bool received_callback = false;

  control_client
      ->CreateRingBuffer({{
          .options = fuchsia_audio_device::RingBufferOptions{{
              .format = fuchsia_audio::Format{{
                  .sample_type = fuchsia_audio::SampleType::kUint8,
                  .channel_count = 1,
                  .frames_per_second = 48000,
              }},
              .ring_buffer_min_bytes = -1u,
          }},
          .ring_buffer_server =
              fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        if (result.is_ok()) {
          FX_LOGS(ERROR) << "RingBufferProperties";
          FX_LOGS(ERROR) << "    valid_bits_per_sample: "
                         << static_cast<int16_t>(
                                result->properties()->valid_bits_per_sample().value_or(-1));
          FX_LOGS(ERROR) << "    turn_on_delay:         "
                         << result->properties()->turn_on_delay().value_or(-1);
          FX_LOGS(ERROR) << "fuchsia.audio.RingBuffer";
          FX_LOGS(ERROR) << "    buffer";
          FX_LOGS(ERROR) << "        vmo:               0x" << std::hex
                         << result->ring_buffer()->buffer()->vmo().get() << " (handle)";
          FX_LOGS(ERROR) << "        size:              0x" << std::hex
                         << result->ring_buffer()->buffer()->size();
          FX_LOGS(ERROR) << "    format";
          FX_LOGS(ERROR) << "        sample_type:       "
                         << fidl::ToUnderlying(*result->ring_buffer()->format()->sample_type());
          FX_LOGS(ERROR) << "        channel_count:     "
                         << *result->ring_buffer()->format()->channel_count();
          FX_LOGS(ERROR) << "        frames_per_second: "
                         << *result->ring_buffer()->format()->frames_per_second();
          FX_LOGS(ERROR)
              << "        channel_layout:    "
              << (result->ring_buffer()->format()->channel_layout().has_value()
                      ? std::to_string(fidl::ToUnderlying(
                            result->ring_buffer()->format()->channel_layout()->config().value()))
                      : "NONE");
          FX_LOGS(ERROR) << "    producer_bytes:        0x" << std::hex
                         << *result->ring_buffer()->producer_bytes();
          FX_LOGS(ERROR) << "    consumer_bytes:        0x" << std::hex
                         << *result->ring_buffer()->consumer_bytes();
          FX_LOGS(ERROR) << "    reference_clock:       0x" << std::hex
                         << result->ring_buffer()->reference_clock()->get() << " (handle)";
          FX_LOGS(ERROR) << "    ref_clock_domain:      "
                         << (result->ring_buffer()->reference_clock_domain().has_value()
                                 ? std::to_string(*result->ring_buffer()->reference_clock_domain())
                                 : "NONE");
        }
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreateRingBufferError::kBadRingBufferOption)
            << result.error_value();

        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferMissingRingBufferServerEnd) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();
  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();

  ASSERT_EQ(ControlServer::count(), 1u);
  bool received_callback = false;

  control_client
      ->CreateRingBuffer({{
          .options = fuchsia_audio_device::RingBufferOptions{{
              .format = fuchsia_audio::Format{{
                  .sample_type = fuchsia_audio::SampleType::kInt16,
                  .channel_count = 2,
                  .frames_per_second = 48000,
              }},
              .ring_buffer_min_bytes = 8192,
          }},
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreateRingBufferError::kInvalidRingBuffer)
            << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

TEST_F(ControlServerStreamConfigWarningTest, CreateRingBufferBadRingBufferServerEnd) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();
  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();

  ASSERT_EQ(ControlServer::count(), 1u);
  bool received_callback = false;

  control_client
      ->CreateRingBuffer({{
          .options = fuchsia_audio_device::RingBufferOptions{{
              .format = fuchsia_audio::Format{{
                  .sample_type = fuchsia_audio::SampleType::kInt16,
                  .channel_count = 2,
                  .frames_per_second = 48000,
              }},
              .ring_buffer_min_bytes = 8192,
          }},
          .ring_buffer_server = fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error()) << result.error_value();
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_INVALID_ARGS)
            << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 0u);
}

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test: device becomes
//   unhealthy before SetGain. Expect Observer/Control/RingBuffer to drop, Reg/WatchRemoved.

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test: device becomes
//   unhealthy before CreateRingBuffer. Expect Obs/Ctl to drop, Reg/WatchRemoved.

TEST_F(ControlServerStreamConfigWarningTest, SetDaiFormatFails) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());
  auto control = CreateTestControlServer(*adr_service_->devices().begin());

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  fuchsia_hardware_audio::DaiFormat dai_format{{
      .number_of_channels = 1,
      .channels_to_use_bitmask = 1,
      .sample_format = fuchsia_hardware_audio::DaiSampleFormat::kPcmSigned,
      .frame_format = fuchsia_hardware_audio::DaiFrameFormat::WithFrameFormatStandard(
          fuchsia_hardware_audio::DaiFrameFormatStandard::kNone),
      .frame_rate = 48000,
      .bits_per_slot = 16,
      .bits_per_sample = 16,
  }};

  control->client()
      ->SetDaiFormat({{.dai_format = dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().framework_error();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlSetDaiFormatError::kWrongDeviceType)
            << result.error_value();
      });
  RunLoopUntilIdle();

  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

TEST_F(ControlServerStreamConfigWarningTest, CodecStartFails) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());
  auto control = CreateTestControlServer(*adr_service_->devices().begin());

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()->CodecStart().Then([&received_callback](
                                           fidl::Result<Control::CodecStart>& result) {
    received_callback = true;
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value().framework_error();
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::ControlCodecStartError::kWrongDeviceType)
        << result.error_value();
  });
  RunLoopUntilIdle();

  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

TEST_F(ControlServerStreamConfigWarningTest, CodecStopFails) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());
  auto control = CreateTestControlServer(*adr_service_->devices().begin());

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()->CodecStop().Then([&received_callback](
                                          fidl::Result<Control::CodecStop>& result) {
    received_callback = true;
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value().framework_error();
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::ControlCodecStopError::kWrongDeviceType)
        << result.error_value();
  });
  RunLoopUntilIdle();

  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

TEST_F(ControlServerStreamConfigWarningTest, CodecResetFails) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());
  auto control = CreateTestControlServer(*adr_service_->devices().begin());

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  control->client()->CodecReset().Then([&received_callback](
                                           fidl::Result<Control::CodecReset>& result) {
    received_callback = true;
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value().framework_error();
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::ControlCodecResetError::kWrongDeviceType)
        << result.error_value();
  });
  RunLoopUntilIdle();

  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

}  // namespace
}  // namespace media_audio
