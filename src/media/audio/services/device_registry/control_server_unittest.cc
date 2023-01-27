// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/control_server.h"

#include <fidl/fuchsia.audio.device/cpp/markers.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/time.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"

namespace media_audio {
namespace {

namespace fidl_adr = fuchsia_audio_device;

class ControlServerTest : public AudioDeviceRegistryServerTestBase {
 protected:
  std::unique_ptr<FakeAudioDriver> CreateAndEnableDriverWithDefaults() {
    EXPECT_EQ(dispatcher(), test_loop().dispatcher());
    zx::channel server_end, client_end;
    EXPECT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
    auto fake_driver = std::make_unique<FakeAudioDriver>(std::move(server_end),
                                                         std::move(client_end), dispatcher());

    adr_service_->AddDevice(Device::Create(
        adr_service_, dispatcher(), "Test output name", fidl_adr::DeviceType::kOutput,
        fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
    RunLoopUntilIdle();
    return fake_driver;
  }

  std::optional<TokenId> WaitForAddedDeviceTokenId(
      fidl::Client<fidl_adr::Registry>& registry_client) {
    std::optional<TokenId> added_device_id;
    registry_client->WatchDevicesAdded().Then(
        [&added_device_id](fidl::Result<fidl_adr::Registry::WatchDevicesAdded>& result) mutable {
          ASSERT_TRUE(result.is_ok());
          ASSERT_TRUE(result->devices());
          ASSERT_EQ(result->devices()->size(), 1u);
          ASSERT_TRUE(result->devices()->at(0).token_id());
          added_device_id = *result->devices()->at(0).token_id();
        });
    RunLoopUntilIdle();
    return added_device_id;
  }

  // Obtain a control via ControlCreator/Create (not the synthetic CreateTestControlServer method).
  fidl::Client<fidl_adr::Control> ConnectToControl(
      fidl::Client<fidl_adr::ControlCreator>& control_creator_client, TokenId token_id) {
    auto [control_client_end, control_server_end] =
        CreateNaturalAsyncClientOrDie<fidl_adr::Control>();
    auto control_client = fidl::Client<fidl_adr::Control>(
        fidl::ClientEnd<fidl_adr::Control>(std::move(control_client_end)), dispatcher(),
        control_fidl_handler_.get());
    bool received_callback = false;
    control_creator_client
        ->Create({{
            .token_id = token_id,
            .control_server = fidl::ServerEnd<fidl_adr::Control>(std::move(control_server_end)),
        }})
        .Then([&received_callback](fidl::Result<fidl_adr::ControlCreator::Create>& result) {
          ASSERT_TRUE(result.is_ok());
          received_callback = true;
        });
    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);
    EXPECT_TRUE(control_client.is_valid());
    return control_client;
  }
};

TEST_F(ControlServerTest, CleanClientDrop) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto control = CreateTestControlServer(*adr_service_->devices().begin());
  RunLoopUntilIdle();
  ASSERT_EQ(ControlServer::count(), 1u);

  control->client() = fidl::Client<fidl_adr::Control>();

  // If Control client doesn't drop cleanly, ControlServer will emit a WARNING, causing a failure.
}

TEST_F(ControlServerTest, CleanServerShutdown) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto control = CreateTestControlServer(*adr_service_->devices().begin());
  RunLoopUntilIdle();
  ASSERT_EQ(ControlServer::count(), 1u);

  control->server().Shutdown(ZX_ERR_PEER_CLOSED);

  // If ControlServer doesn't shutdown cleanly, it emits a WARNING, which will cause a failure.
}

// Same as "CleanClientDrop" test case, but the Control is created "properly" through a
// ControlCreator rather than directly via AudioDeviceRegistry::CreateControlServer.
TEST_F(ControlServerTest, BasicClose) {
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
  control_client = fidl::Client<fidl_adr::Control>();
}

TEST_F(ControlServerTest, ControlCreatorServerShutdownDoesNotAffectControl) {
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
  control_client = fidl::Client<fidl_adr::Control>();
}

TEST_F(ControlServerTest, SetGain) {
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
          .target_state = fidl_adr::GainState{{.gain_db = -1.0f}},
      }})
      .Then([&received_callback](fidl::Result<fidl_adr::Control::SetGain>& result) {
        EXPECT_TRUE(result.is_ok());
        received_callback = true;
      });
  RunLoopUntilIdle();

  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

// Validate that the Control lives, even if the client drops its child RingBuffer.
TEST_F(ControlServerTest, ClientRingBufferDropDoesNotAffectControl) {
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
            .options = fidl_adr::RingBufferOptions{{
                .format = fuchsia_audio::Format{{
                    .sample_type = fuchsia_audio::SampleType::kInt16,
                    .channel_count = 2,
                    .frames_per_second = 48000,
                }},
                .ring_buffer_min_bytes = 2000,
            }},
            .ring_buffer_server =
                fidl::ServerEnd<fidl_adr::RingBuffer>(std::move(ring_buffer_server_end)),
        }})
        .Then([&received_callback](fidl::Result<fidl_adr::Control::CreateRingBuffer>& result) {
          received_callback = true;
          ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        });
    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);

    // Let our RingBuffer client connection drop.
    ring_buffer_client = fidl::Client<fidl_adr::RingBuffer>();
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
TEST_F(ControlServerTest, DriverRingBufferDropDoesNotAffectControl) {
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
          .options = fidl_adr::RingBufferOptions{{
              .format = fuchsia_audio::Format{{
                  .sample_type = fuchsia_audio::SampleType::kInt16,
                  .channel_count = 2,
                  .frames_per_second = 48000,
              }},
              .ring_buffer_min_bytes = 2000,
          }},
          .ring_buffer_server =
              fidl::ServerEnd<fidl_adr::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<fidl_adr::Control::CreateRingBuffer>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
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
TEST_F(ControlServerTest, StreamConfigDropCausesCleanControlServerShutdown) {
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
          .options = fidl_adr::RingBufferOptions{{
              .format = fuchsia_audio::Format{{
                  .sample_type = fuchsia_audio::SampleType::kInt16,
                  .channel_count = 2,
                  .frames_per_second = 48000,
              }},
              .ring_buffer_min_bytes = 2000,
          }},
          .ring_buffer_server =
              fidl::ServerEnd<fidl_adr::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<fidl_adr::Control::CreateRingBuffer>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
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

// TODO(fxbug.dev/117826): unittest GetCurrentlyPermittedFormats

}  // namespace
}  // namespace media_audio
