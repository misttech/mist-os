// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/ring_buffer_server.h"

#include <fidl/fuchsia.audio.device/cpp/markers.h>
#include <fidl/fuchsia.audio/cpp/common_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"

namespace media_audio {
namespace {

namespace fidl_adr = fuchsia_audio_device;

using ::testing::Optional;

class RingBufferServerTest : public AudioDeviceRegistryServerTestBase,
                             public fidl::AsyncEventHandler<fidl_adr::Control>,
                             public fidl::AsyncEventHandler<fidl_adr::RingBuffer> {
 protected:
  static inline const fidl_adr::RingBufferOptions kDefaultRingBufferOptions{{
      .format = fuchsia_audio::Format{{.sample_type = fuchsia_audio::SampleType::kInt16,
                                       .channel_count = 2,
                                       .frames_per_second = 48000}},
      .ring_buffer_min_bytes = 2000,
  }};

  std::unique_ptr<FakeAudioDriver> CreateFakeDriverWithDefaults();
  void EnableDriverAndAddDevice(const std::unique_ptr<FakeAudioDriver>& fake_driver);

  std::pair<fidl::Client<fidl_adr::Registry>, std::shared_ptr<RegistryServer>>
  CreateRegistryServer();
  std::optional<TokenId> WaitForAddedDeviceTokenId(fidl::Client<fidl_adr::Registry>& reg_client);

  std::pair<fidl::Client<fidl_adr::ControlCreator>, std::shared_ptr<ControlCreatorServer>>
  CreateControlCreatorServer();

  fidl::Client<fidl_adr::Control> ConnectToControl(
      fidl::Client<fidl_adr::ControlCreator>& ctl_creator_client, TokenId token_id);

  std::pair<fidl::Client<fidl_adr::RingBuffer>, fidl::ServerEnd<fidl_adr::RingBuffer>>
  CreateRingBufferClient();

  std::pair<fidl::Client<fuchsia_audio_device::Control>,
            fidl::Client<fuchsia_audio_device::RingBuffer>>
  SetupForCleanShutdownTesting();

  // Invoked when the underlying driver disconnects its StreamConfig.
  void on_fidl_error(fidl::UnbindInfo error) override;

 private:
  std::optional<zx_status_t> fidl_error_status_;
};

std::unique_ptr<FakeAudioDriver> RingBufferServerTest::CreateFakeDriverWithDefaults() {
  EXPECT_EQ(dispatcher(), test_loop().dispatcher());
  zx::channel server_end, client_end;
  EXPECT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
  return std::make_unique<FakeAudioDriver>(std::move(server_end), std::move(client_end),
                                           dispatcher());
}
void RingBufferServerTest::EnableDriverAndAddDevice(
    const std::unique_ptr<FakeAudioDriver>& fake_driver) {
  adr_service_->AddDevice(
      Device::Create(adr_service_, dispatcher(), "Test output name", fidl_adr::DeviceType::kOutput,
                     fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
}

std::pair<fidl::Client<fidl_adr::Registry>, std::shared_ptr<RegistryServer>>
RingBufferServerTest::CreateRegistryServer() {
  auto [client_end, server_end] = CreateNaturalAsyncClientOrDie<fidl_adr::Registry>();
  auto server = adr_service_->CreateRegistryServer(std::move(server_end));
  auto client = fidl::Client<fidl_adr::Registry>(std::move(client_end), dispatcher());
  return std::make_pair(std::move(client), server);
}

std::optional<TokenId> RingBufferServerTest::WaitForAddedDeviceTokenId(
    fidl::Client<fidl_adr::Registry>& reg_client) {
  std::optional<TokenId> added_device_id;
  reg_client->WatchDevicesAdded().Then(
      [&added_device_id](fidl::Result<fidl_adr::Registry::WatchDevicesAdded>& result) mutable {
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_device_id = *result->devices()->at(0).token_id();
      });
  RunLoopUntilIdle();
  return added_device_id;
}

std::pair<fidl::Client<fidl_adr::ControlCreator>, std::shared_ptr<ControlCreatorServer>>
RingBufferServerTest::CreateControlCreatorServer() {
  auto [client_end, server_end] = CreateNaturalAsyncClientOrDie<fidl_adr::ControlCreator>();
  auto server = adr_service_->CreateControlCreatorServer(std::move(server_end));
  auto client = fidl::Client<fidl_adr::ControlCreator>(std::move(client_end), dispatcher());
  return std::make_pair(std::move(client), server);
}

fidl::Client<fidl_adr::Control> RingBufferServerTest::ConnectToControl(
    fidl::Client<fidl_adr::ControlCreator>& ctl_creator_client, TokenId token_id) {
  auto [control_client_end, control_server_end] =
      CreateNaturalAsyncClientOrDie<fidl_adr::Control>();
  auto control_client = fidl::Client<fidl_adr::Control>(
      fidl::ClientEnd<fidl_adr::Control>(std::move(control_client_end)), dispatcher(), this);
  bool received_callback = false;
  ctl_creator_client
      ->Create({{
          .token_id = token_id,
          .control_server = fidl::ServerEnd<fidl_adr::Control>(std::move(control_server_end)),
      }})
      .Then([&received_callback](fidl::Result<fidl_adr::ControlCreator::Create>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_TRUE(control_client.is_valid());
  return control_client;
}

std::pair<fidl::Client<fidl_adr::RingBuffer>, fidl::ServerEnd<fidl_adr::RingBuffer>>
RingBufferServerTest::CreateRingBufferClient() {
  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fidl_adr::RingBuffer>();
  auto ring_buffer_client = fidl::Client<fidl_adr::RingBuffer>(
      fidl::ClientEnd<fidl_adr::RingBuffer>(std::move(ring_buffer_client_end)), dispatcher(), this);
  return std::make_pair(std::move(ring_buffer_client), std::move(ring_buffer_server_end));
}

// Invoked when the underlying driver disconnects its StreamConfig.
void RingBufferServerTest::on_fidl_error(fidl::UnbindInfo error) {
  fidl_error_status_ = error.status();
  if (fidl_error_status_ != ZX_OK && fidl_error_status_ != ZX_ERR_PEER_CLOSED) {
    FX_LOGS(WARNING) << __func__ << ":" << error;
  } else {
    FX_LOGS(DEBUG) << __func__ << ":" << error;
  }
}

std::pair<fidl::Client<fuchsia_audio_device::Control>,
          fidl::Client<fuchsia_audio_device::RingBuffer>>
RingBufferServerTest::SetupForCleanShutdownTesting() {
  auto [control_creator_client, control_creator_server] = CreateControlCreatorServer();
  auto [registry_client, registry_server] = CreateRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry_client);
  EXPECT_TRUE(token_id);
  auto control_client = ConnectToControl(control_creator_client, *token_id);

  control_creator_server->Shutdown();
  registry_server->Shutdown();
  RunLoopUntilIdle();
  EXPECT_TRUE(control_creator_server->WaitForShutdown(zx::sec(1)));
  EXPECT_TRUE(registry_server->WaitForShutdown(zx::sec(1)));

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control_client
      ->CreateRingBuffer({{.options = kDefaultRingBufferOptions,
                           .ring_buffer_server = fidl::ServerEnd<fidl_adr::RingBuffer>(
                               std::move(ring_buffer_server_end))}})
      .Then([&received_callback](fidl::Result<fidl_adr::Control::CreateRingBuffer>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  return std::make_pair(std::move(control_client), std::move(ring_buffer_client));
}

// Validate that RingBuffer clients and servers shutdown cleanly (without warnings).
TEST_F(RingBufferServerTest, CleanClientDrop) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);
  auto [control_client, ring_buffer_client] = SetupForCleanShutdownTesting();

  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
  control_client = fidl::Client<fuchsia_audio_device::Control>();
  RunLoopUntilIdle();
}

TEST_F(RingBufferServerTest, CleanServerShutdownAfterDriverRingBufferDrop) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);
  auto [control_client, ring_buffer_client] = SetupForCleanShutdownTesting();

  fake_driver->DropRingBuffer();
  RunLoopUntilIdle();
  control_client = fidl::Client<fuchsia_audio_device::Control>();
  RunLoopUntilIdle();
}

TEST_F(RingBufferServerTest, CleanClientDropAfterControlClientDrop) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);
  auto [control_client, ring_buffer_client] = SetupForCleanShutdownTesting();

  control_client = fidl::Client<fuchsia_audio_device::Control>();
  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
  RunLoopUntilIdle();
}

TEST_F(RingBufferServerTest, CleanServerShutdownAfterStreamConfigDrop) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);
  auto [control_client, ring_buffer_client] = SetupForCleanShutdownTesting();

  fake_driver->DropStreamConfig();
  RunLoopUntilIdle();
}

// Validate that Control/CreateRingBuffer succeeds and returns the expected parameters.
TEST_F(RingBufferServerTest, CreateRingBufferReturnParameters) {
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
              fidl::ServerEnd<fidl_adr::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<fidl_adr::Control::CreateRingBuffer>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();

        ASSERT_TRUE(result->properties());
        EXPECT_THAT(result->properties()->valid_bits_per_sample(), Optional(uint8_t(16)));
        EXPECT_THAT(result->properties()->turn_on_delay(), Optional(0));

        ASSERT_TRUE(result->ring_buffer());
        ASSERT_TRUE(result->ring_buffer()->buffer());
        ASSERT_TRUE(result->ring_buffer()->producer_bytes());
        ASSERT_TRUE(result->ring_buffer()->consumer_bytes());
        ASSERT_TRUE(result->ring_buffer()->reference_clock());
        EXPECT_TRUE(result->ring_buffer()->buffer()->vmo().is_valid());
        EXPECT_GT(result->ring_buffer()->buffer()->size(), 2000u);
        EXPECT_THAT(result->ring_buffer()->format(), kDefaultRingBufferOptions.format());
        EXPECT_EQ(result->ring_buffer()->producer_bytes(), 2000u);
        // consumer_bytes is minimal, based on fifo_depth/internal_delay
        EXPECT_EQ(result->ring_buffer()->consumer_bytes(), 12u);
        EXPECT_TRUE(result->ring_buffer()->reference_clock()->is_valid());
        EXPECT_EQ(result->ring_buffer()->reference_clock_domain().value_or(
                      fuchsia_hardware_audio::kClockDomainMonotonic),
                  fuchsia_hardware_audio::kClockDomainMonotonic);
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
  control_client = fidl::Client<fuchsia_audio_device::Control>();
}

// Validate that RingBuffer/SetActiveChannels succeeds and returns an expected set_time.
TEST_F(RingBufferServerTest, SetActiveChannels) {
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
              fidl::ServerEnd<fidl_adr::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<fidl_adr::Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  EXPECT_TRUE(ring_buffer_client.is_valid());
  received_callback = false;
  auto before_set_active_channels = zx::clock::get_monotonic();
  ring_buffer_client
      ->SetActiveChannels({{
          .channel_bitmask = 0x0,
      }})
      .Then([&received_callback, before_set_active_channels](
                fidl::Result<fidl_adr::RingBuffer::SetActiveChannels>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        ASSERT_TRUE(result->set_time());
        EXPECT_GT(*result->set_time(), before_set_active_channels.get());
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(fake_driver->active_channels_bitmask(), 0x0u);
  EXPECT_GT(fake_driver->active_channels_set_time(), before_set_active_channels);
  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
  control_client = fidl::Client<fuchsia_audio_device::Control>();
  RunLoopUntilIdle();
}

// Validate that RingBuffer/Start and /Stop function as expected, including start_time.
TEST_F(RingBufferServerTest, StartAndStop) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->set_active_channels_supported(false);
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);

  auto [control_creator_client, control_creator_server] = CreateControlCreatorServer();
  auto [registry_client, registry_server] = CreateRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry_client);
  ASSERT_TRUE(token_id);
  auto control_client = ConnectToControl(control_creator_client, *token_id);
  registry_server->Shutdown();
  control_creator_server->Shutdown();

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control_client
      ->CreateRingBuffer({{
          .options = kDefaultRingBufferOptions,
          .ring_buffer_server =
              fidl::ServerEnd<fidl_adr::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<fidl_adr::Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok());
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(fake_driver->is_running());

  received_callback = false;
  auto before_start = zx::clock::get_monotonic();
  ring_buffer_client->Start({}).Then([&received_callback, before_start, &fake_driver](
                                         fidl::Result<fidl_adr::RingBuffer::Start>& result) {
    received_callback = true;
    ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
    EXPECT_THAT(result->start_time(), Optional(fake_driver->mono_start_time().get()));
    EXPECT_GT(*result->start_time(), before_start.get());
    EXPECT_TRUE(fake_driver->is_running());
  });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  received_callback = false;
  ring_buffer_client
      ->SetActiveChannels({{
          .channel_bitmask = 0x0,
      }})
      .Then([&received_callback](fidl::Result<fidl_adr::RingBuffer::SetActiveChannels>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_error());
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  received_callback = false;
  ring_buffer_client->Stop({}).Then(
      [&received_callback, &fake_driver](fidl::Result<fidl_adr::RingBuffer::Stop>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        EXPECT_FALSE(fake_driver->is_running());
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
  control_client = fidl::Client<fuchsia_audio_device::Control>();
  RunLoopUntilIdle();
  EXPECT_TRUE(control_creator_server->WaitForShutdown(zx::sec(1)));
  EXPECT_TRUE(registry_server->WaitForShutdown(zx::sec(1)));
}

// Validate that RingBuffer/WatchDelayInfo notifies of the delay received during initialization.
TEST_F(RingBufferServerTest, WatchDelayInfo) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->InjectDelayUpdate(zx::nsec(987'654'321), zx::nsec(123'456'789));
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);

  auto [control_creator_client, control_creator_server] = CreateControlCreatorServer();
  auto [registry_client, registry_server] = CreateRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry_client);
  ASSERT_TRUE(token_id);
  auto control_client = ConnectToControl(control_creator_client, *token_id);
  registry_server->Shutdown();
  control_creator_server->Shutdown();

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control_client
      ->CreateRingBuffer({{
          .options = kDefaultRingBufferOptions,
          .ring_buffer_server =
              fidl::ServerEnd<fidl_adr::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<fidl_adr::Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(fake_driver->is_running());

  received_callback = false;
  ring_buffer_client->WatchDelayInfo().Then(
      [&received_callback](fidl::Result<fidl_adr::RingBuffer::WatchDelayInfo>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        ASSERT_TRUE(result->delay_info());
        ASSERT_TRUE(result->delay_info());
        ASSERT_TRUE(result->delay_info());
        EXPECT_THAT(result->delay_info()->internal_delay(), Optional(987'654'321u));
        EXPECT_THAT(result->delay_info()->external_delay(), Optional(123'456'789u));
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
  control_client = fidl::Client<fuchsia_audio_device::Control>();
  RunLoopUntilIdle();
  EXPECT_TRUE(control_creator_server->WaitForShutdown(zx::sec(1)));
  EXPECT_TRUE(registry_server->WaitForShutdown(zx::sec(1)));
}

// Validate that RingBuffer/WatchDelayInfo notifies of delay changes after initialization.
TEST_F(RingBufferServerTest, DynamicDelayUpdate) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);

  auto [control_creator_client, control_creator_server] = CreateControlCreatorServer();
  auto [registry_client, registry_server] = CreateRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry_client);
  ASSERT_TRUE(token_id);
  auto control_client = ConnectToControl(control_creator_client, *token_id);
  registry_server->Shutdown();
  control_creator_server->Shutdown();

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control_client
      ->CreateRingBuffer({{
          .options = kDefaultRingBufferOptions,
          .ring_buffer_server =
              fidl::ServerEnd<fidl_adr::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<fidl_adr::Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(fake_driver->is_running());

  received_callback = false;
  ring_buffer_client->WatchDelayInfo().Then(
      [&received_callback](fidl::Result<fidl_adr::RingBuffer::WatchDelayInfo>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        ASSERT_TRUE(result->delay_info());
        ASSERT_FALSE(result->delay_info()->external_delay());
        EXPECT_THAT(result->delay_info()->internal_delay(), Optional(62500u));
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  received_callback = false;
  ring_buffer_client->WatchDelayInfo().Then(
      [&received_callback](fidl::Result<fidl_adr::RingBuffer::WatchDelayInfo>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        ASSERT_TRUE(result->delay_info());
        ASSERT_TRUE(result->delay_info());
        EXPECT_THAT(result->delay_info()->internal_delay(), Optional(987'654'321u));
        EXPECT_THAT(result->delay_info()->external_delay(), Optional(123'456'789u));
      });
  RunLoopUntilIdle();
  EXPECT_FALSE(received_callback);

  fake_driver->InjectDelayUpdate(zx::nsec(987'654'321), zx::nsec(123'456'789));
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
  control_client = fidl::Client<fuchsia_audio_device::Control>();
  RunLoopUntilIdle();
  EXPECT_TRUE(control_creator_server->WaitForShutdown(zx::sec(1)));
  EXPECT_TRUE(registry_server->WaitForShutdown(zx::sec(1)));
}

}  // namespace
}  // namespace media_audio
