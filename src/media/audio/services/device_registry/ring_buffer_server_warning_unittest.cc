// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/markers.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/ring_buffer_server.h"

namespace media_audio {
namespace {

namespace fidl_device = fuchsia_audio_device;

class RingBufferServerWarningTest : public AudioDeviceRegistryServerTestBase,
                                    public fidl::AsyncEventHandler<fidl_device::Control>,
                                    public fidl::AsyncEventHandler<fidl_device::RingBuffer> {
 protected:
  const fidl_device::RingBufferOptions kDefaultRingBufferOptions = {{
      .format = fuchsia_audio::Format{{
          .sample_type = fuchsia_audio::SampleType::kInt16,
          .channel_count = 2,
          .frames_per_second = 48000,
      }},
      .ring_buffer_min_bytes = 2000,
  }};

  std::unique_ptr<FakeAudioDriver> CreateFakeDriverWithDefaults();
  void EnableDriverAndAddDevice(const std::unique_ptr<FakeAudioDriver>& fake_driver);

  std::pair<fidl::Client<fidl_device::Registry>, std::shared_ptr<RegistryServer>>
  CreateRegistryServer();
  std::optional<TokenId> WaitForAddedDeviceTokenId(fidl::Client<fidl_device::Registry>& reg_client);

  std::pair<fidl::Client<fidl_device::ControlCreator>, std::shared_ptr<ControlCreatorServer>>
  CreateControlCreatorServer();

  fidl::Client<fidl_device::Control> ConnectToControl(
      fidl::Client<fidl_device::ControlCreator>& ctl_creator_client, TokenId token_id);

  std::pair<fidl::Client<fidl_device::RingBuffer>, fidl::ServerEnd<fidl_device::RingBuffer>>
  CreateRingBufferClient();

  // Invoked when the underlying driver disconnects its StreamConfig.
  void on_fidl_error(fidl::UnbindInfo error) override;

 private:
  std::optional<zx_status_t> fidl_error_status_;
};

std::unique_ptr<FakeAudioDriver> RingBufferServerWarningTest::CreateFakeDriverWithDefaults() {
  EXPECT_EQ(dispatcher(), test_loop().dispatcher());
  zx::channel server_end, client_end;
  EXPECT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
  return std::make_unique<FakeAudioDriver>(std::move(server_end), std::move(client_end),
                                           dispatcher());
}
void RingBufferServerWarningTest::EnableDriverAndAddDevice(
    const std::unique_ptr<FakeAudioDriver>& fake_driver) {
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fidl_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
}

std::pair<fidl::Client<fidl_device::Registry>, std::shared_ptr<RegistryServer>>
RingBufferServerWarningTest::CreateRegistryServer() {
  auto [client_end, server_end] = CreateNaturalAsyncClientOrDie<fidl_device::Registry>();
  auto server = adr_service_->CreateRegistryServer(std::move(server_end));
  auto client = fidl::Client<fidl_device::Registry>(std::move(client_end), dispatcher());
  return std::make_pair(std::move(client), server);
}

std::optional<TokenId> RingBufferServerWarningTest::WaitForAddedDeviceTokenId(
    fidl::Client<fidl_device::Registry>& reg_client) {
  std::optional<TokenId> added_device_id;
  reg_client->WatchDevicesAdded().Then(
      [&added_device_id](fidl::Result<fidl_device::Registry::WatchDevicesAdded>& result) mutable {
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_device_id = *result->devices()->at(0).token_id();
      });
  RunLoopUntilIdle();
  return added_device_id;
}

std::pair<fidl::Client<fidl_device::ControlCreator>, std::shared_ptr<ControlCreatorServer>>
RingBufferServerWarningTest::CreateControlCreatorServer() {
  auto [client_end, server_end] = CreateNaturalAsyncClientOrDie<fidl_device::ControlCreator>();
  auto server = adr_service_->CreateControlCreatorServer(std::move(server_end));
  auto client = fidl::Client<fidl_device::ControlCreator>(std::move(client_end), dispatcher());
  return std::make_pair(std::move(client), server);
}

fidl::Client<fidl_device::Control> RingBufferServerWarningTest::ConnectToControl(
    fidl::Client<fidl_device::ControlCreator>& ctl_creator_client, TokenId token_id) {
  auto [control_client_end, control_server_end] =
      CreateNaturalAsyncClientOrDie<fidl_device::Control>();
  auto control_client = fidl::Client<fidl_device::Control>(
      fidl::ClientEnd<fidl_device::Control>(std::move(control_client_end)), dispatcher(), this);
  bool received_callback = false;
  ctl_creator_client
      ->Create({{
          .token_id = token_id,
          .control_server = fidl::ServerEnd<fidl_device::Control>(std::move(control_server_end)),
      }})
      .Then([&received_callback](fidl::Result<fidl_device::ControlCreator::Create>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_TRUE(control_client.is_valid());
  return control_client;
}

std::pair<fidl::Client<fidl_device::RingBuffer>, fidl::ServerEnd<fidl_device::RingBuffer>>
RingBufferServerWarningTest::CreateRingBufferClient() {
  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fidl_device::RingBuffer>();
  auto ring_buffer_client = fidl::Client<fidl_device::RingBuffer>(
      fidl::ClientEnd<fidl_device::RingBuffer>(std::move(ring_buffer_client_end)), dispatcher(),
      this);
  return std::make_pair(std::move(ring_buffer_client), std::move(ring_buffer_server_end));
}

// Invoked when the underlying driver disconnects its StreamConfig.
void RingBufferServerWarningTest::on_fidl_error(fidl::UnbindInfo error) {
  fidl_error_status_ = error.status();
  if (fidl_error_status_ != ZX_OK && fidl_error_status_ != ZX_ERR_PEER_CLOSED) {
    FX_LOGS(WARNING) << __func__ << ":" << error;
  } else {
    FX_LOGS(INFO) << __func__ << ":" << error;
  }
}

TEST_F(RingBufferServerWarningTest, SetActiveChannelsMissingChannelBitmask) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);

  auto [control_creator_client, control_creator_server] = CreateControlCreatorServer();
  auto [registry_client, registry_server] = CreateRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry_client);
  ASSERT_TRUE(token_id);
  auto control_client = ConnectToControl(control_creator_client, *token_id);

  control_creator_server->Shutdown();
  registry_server->Shutdown();
  RunLoopUntilIdle();
  EXPECT_TRUE(control_creator_server->WaitForShutdown(zx::sec(1)));
  EXPECT_TRUE(registry_server->WaitForShutdown(zx::sec(1)));

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control_client
      ->CreateRingBuffer({{
          .options = kDefaultRingBufferOptions,
          .ring_buffer_server =
              fidl::ServerEnd<fidl_device::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<fidl_device::Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  EXPECT_TRUE(ring_buffer_client.is_valid());
  received_callback = false;
  // No `channel_bitmask` value is included in this call.
  ring_buffer_client->SetActiveChannels({}).Then(
      [&received_callback](fidl::Result<fidl_device::RingBuffer::SetActiveChannels>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error()) << result.error_value().FormatDescription();
        ASSERT_TRUE(result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fidl_device::RingBufferSetActiveChannelsError::kInvalidChannelBitmask);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(fake_driver->active_channels_bitmask(), 0x3u);
  control_client = fidl::Client<fuchsia_audio_device::Control>();
  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
  RunLoopUntilIdle();
}

TEST_F(RingBufferServerWarningTest, SetActiveChannelsBadChannelBitmask) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);

  auto [control_creator_client, control_creator_server] = CreateControlCreatorServer();
  auto [registry_client, registry_server] = CreateRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry_client);
  ASSERT_TRUE(token_id);
  auto control_client = ConnectToControl(control_creator_client, *token_id);

  control_creator_server->Shutdown();
  registry_server->Shutdown();
  RunLoopUntilIdle();
  EXPECT_TRUE(control_creator_server->WaitForShutdown(zx::sec(1)));
  EXPECT_TRUE(registry_server->WaitForShutdown(zx::sec(1)));

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control_client
      ->CreateRingBuffer({{
          .options = kDefaultRingBufferOptions,
          .ring_buffer_server =
              fidl::ServerEnd<fidl_device::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<fidl_device::Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  EXPECT_TRUE(ring_buffer_client.is_valid());
  received_callback = false;
  ring_buffer_client
      ->SetActiveChannels({{
          0xFFFF,  // This channel bitmask includes values outside the total number of channels.
      }})
      .Then([&received_callback](fidl::Result<fidl_device::RingBuffer::SetActiveChannels>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error()) << result.error_value().FormatDescription();
        ASSERT_TRUE(result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fidl_device::RingBufferSetActiveChannelsError::kChannelOutOfRange);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(fake_driver->active_channels_bitmask(), 0x3u);
  control_client = fidl::Client<fuchsia_audio_device::Control>();
  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
  RunLoopUntilIdle();
}

TEST_F(RingBufferServerWarningTest, SetActiveChannelsUnsupported) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  // Set this fake hardware to NOT support powering-down channels.
  fake_driver->set_active_channels_supported(false);
  EnableDriverAndAddDevice(fake_driver);

  auto [control_creator_client, control_creator_server] = CreateControlCreatorServer();
  auto [registry_client, registry_server] = CreateRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry_client);
  ASSERT_TRUE(token_id);
  auto control_client = ConnectToControl(control_creator_client, *token_id);

  control_creator_server->Shutdown();
  registry_server->Shutdown();
  RunLoopUntilIdle();
  EXPECT_TRUE(control_creator_server->WaitForShutdown(zx::sec(1)));
  EXPECT_TRUE(registry_server->WaitForShutdown(zx::sec(1)));

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control_client
      ->CreateRingBuffer({{
          .options = kDefaultRingBufferOptions,
          .ring_buffer_server =
              fidl::ServerEnd<fidl_device::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<fidl_device::Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_TRUE(ring_buffer_client.is_valid());
  received_callback = false;
  ring_buffer_client->SetActiveChannels({{0}}).Then(
      [&received_callback](fidl::Result<fidl_device::RingBuffer::SetActiveChannels>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error()) << result.error_value().FormatDescription();
        ASSERT_TRUE(result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fidl_device::RingBufferSetActiveChannelsError::kMethodNotSupported);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(fake_driver->active_channels_bitmask(), 0x3u);
  control_client = fidl::Client<fuchsia_audio_device::Control>();
  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
  RunLoopUntilIdle();
}

// TODO(fxbug/dev:117199): When Health can change post-initialization, test: Healthy device becomes
//   unhealthy before SetActiveChannels. Expect Obs/Ctl/RB to drop & Reg/WatchRemoved.

// TODO(fxbug/dev:117199): When Health can change post-initialization, test: Healthy device becomes
//   unhealthy before Start. Expect Obs/Ctl/RB to drop & Reg/WatchRemoved.

// TODO(fxbug/dev:117199): When Health can change post-initialization, test: Healthy device becomes
//   unhealthy before Stop. Expect Obs/Ctl/RB to drop & Reg/WatchRemoved.

// TODO(fxbug/dev:117199): When Health can change post-initialization, test: Healthy device becomes
//   unhealthy before WatchDelayInfo. Expect Obs/Ctl/RB to drop & Reg/WatchRemoved.

// TODO(fxbug.dev/117826): Test Start when already Started

// TODO(fxbug.dev/117826): Test Stop when not yet Started, or after Start-then-Stop.

// TODO(fxbug.dev/117826): Test WatchDelayInfo when already watching

}  // namespace
}  // namespace media_audio
