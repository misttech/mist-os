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

class ControlServerTest : public AudioDeviceRegistryServerTestBase {
 protected:
  std::unique_ptr<FakeAudioDriver> CreateAndEnableDriverWithDefaults() {
    EXPECT_EQ(dispatcher(), test_loop().dispatcher());
    zx::channel server_end, client_end;
    EXPECT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
    auto fake_driver = std::make_unique<FakeAudioDriver>(std::move(server_end),
                                                         std::move(client_end), dispatcher());

    adr_service_->AddDevice(Device::Create(
        adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
        fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
    RunLoopUntilIdle();
    return fake_driver;
  }

  std::optional<TokenId> WaitForAddedDeviceTokenId(
      fidl::Client<fuchsia_audio_device::Registry>& registry_client) {
    std::optional<TokenId> added_device_id;
    registry_client->WatchDevicesAdded().Then(
        [&added_device_id](
            fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
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
  fidl::Client<fuchsia_audio_device::Control> ConnectToControl(
      fidl::Client<fuchsia_audio_device::ControlCreator>& control_creator_client,
      TokenId token_id) {
    auto [control_client_end, control_server_end] =
        CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Control>();
    auto control_client = fidl::Client<fuchsia_audio_device::Control>(
        fidl::ClientEnd<fuchsia_audio_device::Control>(std::move(control_client_end)), dispatcher(),
        control_fidl_handler_.get());
    bool received_callback = false;
    control_creator_client
        ->Create({{
            .token_id = token_id,
            .control_server =
                fidl::ServerEnd<fuchsia_audio_device::Control>(std::move(control_server_end)),
        }})
        .Then([&received_callback](
                  fidl::Result<fuchsia_audio_device::ControlCreator::Create>& result) {
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

  control->client() = fidl::Client<fuchsia_audio_device::Control>();
}

TEST_F(ControlServerTest, CleanServerShutdown) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto control = CreateTestControlServer(*adr_service_->devices().begin());
  RunLoopUntilIdle();
  ASSERT_EQ(ControlServer::count(), 1u);

  control->server().Shutdown(ZX_ERR_PEER_CLOSED);
}

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
  control_client = fidl::Client<fuchsia_audio_device::Control>();
}

TEST_F(ControlServerTest, ControlOutlivesControlCreator) {
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
  control_client = fidl::Client<fuchsia_audio_device::Control>();
}

TEST_F(ControlServerTest, SetGain) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();
  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  auto received_callback = false;
  control_client
      ->SetGain({{
          .target_state = fuchsia_audio_device::GainState{{.gain_db = -1.0f}},
      }})
      .Then([&received_callback](fidl::Result<fuchsia_audio_device::Control::SetGain>& result) {
        EXPECT_TRUE(result.is_ok());
        received_callback = true;
      });
  RunLoopUntilIdle();

  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
  control_client = fidl::Client<fuchsia_audio_device::Control>();
}

TEST_F(ControlServerTest, CreateRingBuffer) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  auto registry = CreateTestRegistryServer();
  auto added_id = WaitForAddedDeviceTokenId(registry->client());
  auto control_creator = CreateTestControlCreatorServer();
  auto control_client = ConnectToControl(control_creator->client(), *added_id);
  RunLoopUntilIdle();

  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  ASSERT_EQ(ControlServer::count(), 1u);

  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
      fidl::ClientEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_client_end)),
      dispatcher(), ring_buffer_fidl_handler_.get());
  bool received_callback = false;
  control_client
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
      .Then([&received_callback](
                fidl::Result<fuchsia_audio_device::Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok());
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
  control_client = fidl::Client<fuchsia_audio_device::Control>();
}

// TODO(fxbug.dev/117826): TEST_F(ControlServerTest, ControlLivesIfClientDropsRingBuffer)
// TODO(fxbug.dev/117826): TEST_F(ControlServerTest, ControlLivesIfDriverDropsRingBuffer)
// TODO(fxbug.dev/117826): unittest GetCurrentlyPermittedFormats

}  // namespace
}  // namespace media_audio
