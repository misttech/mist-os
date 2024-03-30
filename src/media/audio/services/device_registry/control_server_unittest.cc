// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/control_server.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/natural_types.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/common_unittest.h"
#include "src/media/audio/services/device_registry/testing/fake_codec.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"

namespace media_audio {
namespace {

using Control = fuchsia_audio_device::Control;
using DriverClient = fuchsia_audio_device::DriverClient;

class ControlServerTest : public AudioDeviceRegistryServerTestBase {
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

class ControlServerCodecTest : public ControlServerTest {
 protected:
  std::unique_ptr<FakeCodec> CreateAndEnableDriverWithDefaults() {
    auto fake_driver = CreateFakeCodecOutput();

    adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test codec name",
                                           fuchsia_audio_device::DeviceType::kCodec,
                                           DriverClient::WithCodec(fake_driver->Enable())));
    RunLoopUntilIdle();
    return fake_driver;
  }
};

class ControlServerStreamConfigTest : public ControlServerTest {
 protected:
  std::unique_ptr<FakeStreamConfig> CreateAndEnableDriverWithDefaults() {
    auto fake_driver = CreateFakeStreamConfigOutput();

    adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test output name",
                                           fuchsia_audio_device::DeviceType::kOutput,
                                           DriverClient::WithStreamConfig(fake_driver->Enable())));
    RunLoopUntilIdle();
    return fake_driver;
  }
};

/////////////////////
// Codec tests
//
// When client drops their Control, the server should cleanly unwind without hang or WARNING.
TEST_F(ControlServerCodecTest, CleanClientDrop) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto control = CreateTestControlServer(*adr_service_->devices().begin());
  RunLoopUntilIdle();
  ASSERT_EQ(ControlServer::count(), 1u);

  (void)control->client().UnbindMaybeGetEndpoint();
  // If Control client doesn't drop cleanly, ControlServer will emit a WARNING, causing a failure.
}

// When server closes a client connection, the shutdown should be orderly without hang or WARNING.
TEST_F(ControlServerCodecTest, CleanServerShutdown) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto control = CreateTestControlServer(*adr_service_->devices().begin());
  RunLoopUntilIdle();
  ASSERT_EQ(ControlServer::count(), 1u);

  control->server().Shutdown(ZX_ERR_PEER_CLOSED);

  // If ControlServer doesn't shutdown cleanly, it emits a WARNING, which will cause a failure.
}

// When client drops their Control, the server should cleanly unwind without hang or WARNING.
//
// (Same as "CleanClientDrop" test case, but the Control is created "properly" through a
// ControlCreator rather than directly via AudioDeviceRegistry::CreateControlServer.)
TEST_F(ControlServerCodecTest, BasicClose) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();

  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  RunLoopUntilIdle();
  EXPECT_TRUE(control_client.is_valid());
  (void)control_client.UnbindMaybeGetEndpoint();
}

// A ControlCreator can be closed without affecting the Controls that it created.
TEST_F(ControlServerCodecTest, ControlCreatorServerShutdownDoesNotAffectControl) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();

  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  control_creator->server().Shutdown(ZX_ERR_PEER_CLOSED);
  RunLoopUntilIdle();
  EXPECT_TRUE(control_creator->server().WaitForShutdown(zx::sec(1)));

  EXPECT_TRUE(control_client.is_valid());
  EXPECT_EQ(ControlServer::count(), 1u);
  (void)control_client.UnbindMaybeGetEndpoint();
}

// Validate that the ControlServer shuts down cleanly if the driver drops its Codec.
TEST_F(ControlServerCodecTest, CodecDropCausesCleanControlServerShutdown) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  WaitForAddedDeviceTokenId(registry->client());
  auto control = CreateTestControlServer(*adr_service_->devices().begin());

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  // Drop the driver StreamConfig connection.
  fake_driver->DropCodec();

  RunLoopUntilIdle();
  EXPECT_TRUE(control->server().WaitForShutdown(zx::sec(5)));
}

// Validate basic SetDaiFormat functionality, including valid CodecFormatInfo returned.
TEST_F(ControlServerCodecTest, SetDaiFormat) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service_->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;

  // Determine a safe DaiFormat to set
  auto dai_format = SafeDaiFormatFromDaiSupportedFormats(device->dai_format_sets());
  control->client()
      ->SetDaiFormat({{.dai_format = dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->state());
        EXPECT_EQ(ValidateCodecFormatInfo(*result->state()), ZX_OK);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

// Validate basic CodecStart functionality including a current start_time.
TEST_F(ControlServerCodecTest, CodecStart) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());
  auto device = *adr_service_->devices().begin();
  auto control = CreateTestControlServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);
  auto received_callback = false;
  auto dai_format = SafeDaiFormatFromDaiSupportedFormats(device->dai_format_sets());
  control->client()
      ->SetDaiFormat({{.dai_format = dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  received_callback = false;
  auto start_time = zx::time::infinite_past();
  auto time_before_start = zx::clock::get_monotonic();

  control->client()->CodecStart().Then(
      [&received_callback, &start_time](fidl::Result<Control::CodecStart>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->start_time().has_value());
        start_time = zx::time(*result->start_time());
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_GT(start_time.get(), time_before_start.get());
  EXPECT_EQ(ControlServer::count(), 1u);
}

// Validate basic CodecStop functionality including a current stop_time.
TEST_F(ControlServerCodecTest, CodecStop) {
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
  auto stop_time = zx::time::infinite_past();
  auto time_before_stop = zx::clock::get_monotonic();
  received_callback = false;

  control->client()->CodecStop().Then(
      [&received_callback, &stop_time](fidl::Result<Control::CodecStop>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->stop_time().has_value());
        stop_time = zx::time(*result->stop_time());
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_GT(stop_time.get(), time_before_stop.get());
  EXPECT_EQ(ControlServer::count(), 1u);
}

// CodecReset - validate that the DaiFormat and the Start state are reset.
TEST_F(ControlServerCodecTest, CodecReset) {
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
  zx::time first_start_time;
  control->client()->CodecStart().Then(
      [&received_callback, &first_start_time](fidl::Result<Control::CodecStart>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->start_time());
        first_start_time = zx::time(*result->start_time());
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  received_callback = false;

  control->client()->CodecReset().Then(
      [&received_callback](fidl::Result<Control::CodecReset>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  // Only way to verify that DaiFormat is reset: set the same format again.
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  received_callback = false;
  control->client()
      ->SetDaiFormat({{.dai_format = dai_format}})
      .Then([&received_callback](fidl::Result<Control::SetDaiFormat>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  // Only way to verify that Start state is reset: call CodecStart again.
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  received_callback = false;
  zx::time second_start_time;
  control->client()->CodecStart().Then(
      [&received_callback, &second_start_time](fidl::Result<Control::CodecStart>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->start_time());
        second_start_time = zx::time(*result->start_time());
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_GT(second_start_time.get(), first_start_time.get());
  EXPECT_EQ(ControlServer::count(), 1u);
}

// Add test cases for SetTopology and SetElementState (once implemented)
//
// TODO(https://fxbug.dev/323270827): implement signalprocessing for Codec (topology, gain).

/////////////////////
// StreamConfig tests
//
// When client drops their Control, the server should cleanly unwind without hang or WARNING.
TEST_F(ControlServerStreamConfigTest, CleanClientDrop) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto control = CreateTestControlServer(*adr_service_->devices().begin());
  RunLoopUntilIdle();
  ASSERT_EQ(ControlServer::count(), 1u);

  (void)control->client().UnbindMaybeGetEndpoint();

  // If Control client doesn't drop cleanly, ControlServer will emit a WARNING, causing a failure.
}

// When server closes a client connection, the shutdown should be orderly without hang or WARNING.
TEST_F(ControlServerStreamConfigTest, CleanServerShutdown) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto control = CreateTestControlServer(*adr_service_->devices().begin());
  RunLoopUntilIdle();
  ASSERT_EQ(ControlServer::count(), 1u);

  control->server().Shutdown(ZX_ERR_PEER_CLOSED);

  // If ControlServer doesn't shutdown cleanly, it emits a WARNING, which will cause a failure.
}

// When client drops their Control, the server should cleanly unwind without hang or WARNING.
//
// (Same as "CleanClientDrop" test case, but the Control is created "properly" through a
// ControlCreator rather than directly via AudioDeviceRegistry::CreateControlServer.)
TEST_F(ControlServerStreamConfigTest, BasicClose) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();

  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  RunLoopUntilIdle();
  EXPECT_TRUE(control_client.is_valid());
  (void)control_client.UnbindMaybeGetEndpoint();
}

// A ControlCreator can be closed without affecting the Controls that it created.
TEST_F(ControlServerStreamConfigTest, ControlCreatorServerShutdownDoesNotAffectControl) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();
  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();

  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  control_creator->server().Shutdown(ZX_ERR_PEER_CLOSED);
  RunLoopUntilIdle();
  EXPECT_TRUE(control_creator->server().WaitForShutdown(zx::sec(1)));

  EXPECT_TRUE(control_client.is_valid());
  EXPECT_EQ(ControlServer::count(), 1u);
  (void)control_client.UnbindMaybeGetEndpoint();
}

// Validate that the ControlServer shuts down cleanly if the driver drops its Codec.
TEST_F(ControlServerStreamConfigTest, SetGain) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();
  (void)WaitForAddedDeviceTokenId(registry->client());

  auto control = CreateTestControlServer(*adr_service_->devices().begin());

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  auto received_callback = false;

  control->client()
      ->SetGain({{
          .target_state = fuchsia_audio_device::GainState{{.gain_db = -1.0f}},
      }})
      .Then([&received_callback](fidl::Result<Control::SetGain>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });
  RunLoopUntilIdle();

  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

// Validate that the Control lives, even if the client drops its child RingBuffer.
TEST_F(ControlServerStreamConfigTest, ClientRingBufferDropDoesNotAffectControl) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();

  WaitForAddedDeviceTokenId(registry->client());

  auto control = CreateTestControlServer(*adr_service_->devices().begin());

  RunLoopUntilIdle();

  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  {
    auto [ring_buffer_client_end, ring_buffer_server_end] =
        CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
    bool received_callback = false;

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
            .ring_buffer_server = fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(
                std::move(ring_buffer_server_end)),
        }})
        .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
          ASSERT_TRUE(result.is_ok()) << result.error_value();
          received_callback = true;
        });
    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);

    // Let our RingBuffer client connection drop.
    (void)ring_buffer_client.UnbindMaybeGetEndpoint();
  }

  // Wait for the RingBufferServer to destruct.
  while (RingBufferServer::count() > 0u) {
    RunLoopUntilIdle();
  }

  // Allow the ControlServer to destruct, if it (erroneously) wants to.
  RunLoopUntilIdle();
  EXPECT_TRUE(control->client().is_valid());
  EXPECT_EQ(ControlServer::count(), 1u);
}

// Validate that the Control lives, even if the driver drops its RingBuffer connection.
TEST_F(ControlServerStreamConfigTest, DriverRingBufferDropDoesNotAffectControl) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();

  WaitForAddedDeviceTokenId(registry->client());

  auto control = CreateTestControlServer(*adr_service_->devices().begin());

  RunLoopUntilIdle();

  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  auto [ring_buffer_client, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  bool received_callback = false;

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
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_EQ(RingBufferServer::count(), 1u);
  EXPECT_TRUE(received_callback);

  // Drop the driver RingBuffer connection.
  fake_driver->DropRingBuffer();

  // Wait for the RingBufferServer to destruct.
  while (RingBufferServer::count() > 0u) {
    RunLoopUntilIdle();
  }

  // Allow the ControlServer to destruct, if it (erroneously) wants to.
  RunLoopUntilIdle();
  EXPECT_TRUE(control->client().is_valid());
  EXPECT_EQ(ControlServer::count(), 1u);
}

// Validate that the ControlServer shuts down cleanly if the driver drops its StreamConfig.
TEST_F(ControlServerStreamConfigTest, StreamConfigDropCausesCleanControlServerShutdown) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();

  WaitForAddedDeviceTokenId(registry->client());

  auto control = CreateTestControlServer(*adr_service_->devices().begin());

  RunLoopUntilIdle();

  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  auto [ring_buffer_client, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  bool received_callback = false;

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
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_EQ(RingBufferServer::count(), 1u);
  EXPECT_TRUE(received_callback);

  // Drop the driver StreamConfig connection.
  fake_driver->DropStreamConfig();
  RunLoopUntilIdle();

  while (RingBufferServer::count() > 0u) {
    RunLoopUntilIdle();
  }
  EXPECT_TRUE(control->server().WaitForShutdown(zx::sec(5)));
}

}  // namespace
}  // namespace media_audio
