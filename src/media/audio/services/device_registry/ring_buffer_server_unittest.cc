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

#include "src/media/audio/services/common/testing/test_server_and_async_client.h"
#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"

namespace media_audio {
namespace {

using ::testing::Optional;
using Control = fuchsia_audio_device::Control;
using RingBuffer = fuchsia_audio_device::RingBuffer;

class RingBufferServerTest : public AudioDeviceRegistryServerTestBase,
                             public fidl::AsyncEventHandler<fuchsia_audio_device::RingBuffer> {
 protected:
  static inline const fuchsia_audio_device::RingBufferOptions kDefaultRingBufferOptions{{
      .format = fuchsia_audio::Format{{.sample_type = fuchsia_audio::SampleType::kInt16,
                                       .channel_count = 2,
                                       .frames_per_second = 48000}},
      .ring_buffer_min_bytes = 2000,
  }};

  std::unique_ptr<FakeAudioDriver> CreateFakeDriverWithDefaults();
  void EnableDriverAndAddDevice(const std::unique_ptr<FakeAudioDriver>& fake_driver);

  std::optional<TokenId> WaitForAddedDeviceTokenId(
      fidl::Client<fuchsia_audio_device::Registry>& reg_client);

  std::pair<fidl::Client<fuchsia_audio_device::RingBuffer>,
            fidl::ServerEnd<fuchsia_audio_device::RingBuffer>>
  CreateRingBufferClient();

  std::pair<std::unique_ptr<TestServerAndNaturalAsyncClient<ControlServer>>,
            fidl::Client<fuchsia_audio_device::RingBuffer>>
  SetupForCleanShutdownTesting();
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
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
}

std::optional<TokenId> RingBufferServerTest::WaitForAddedDeviceTokenId(
    fidl::Client<fuchsia_audio_device::Registry>& reg_client) {
  std::optional<TokenId> added_device_id;
  reg_client->WatchDevicesAdded().Then(
      [&added_device_id](
          fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_device_id = *result->devices()->at(0).token_id();
      });
  RunLoopUntilIdle();
  return added_device_id;
}

std::pair<fidl::Client<fuchsia_audio_device::RingBuffer>,
          fidl::ServerEnd<fuchsia_audio_device::RingBuffer>>
RingBufferServerTest::CreateRingBufferClient() {
  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
      fidl::ClientEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_client_end)),
      dispatcher(), this);
  return std::make_pair(std::move(ring_buffer_client), std::move(ring_buffer_server_end));
}

std::pair<std::unique_ptr<TestServerAndNaturalAsyncClient<ControlServer>>,
          fidl::Client<fuchsia_audio_device::RingBuffer>>
RingBufferServerTest::SetupForCleanShutdownTesting() {
  auto registry = CreateTestRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  FX_CHECK(token_id);
  auto control_creator = CreateTestControlCreatorServer();

  auto [presence, device_to_control] = adr_service_->FindDeviceByTokenId(*token_id);
  FX_CHECK(presence == AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(device_to_control);

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control->client()
      ->CreateRingBuffer({{.options = kDefaultRingBufferOptions,
                           .ring_buffer_server = fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(
                               std::move(ring_buffer_server_end))}})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
      });
  RunLoopUntilIdle();
  FX_CHECK(received_callback);

  return std::make_pair(std::move(control), std::move(ring_buffer_client));
}

// Validate that RingBuffer clients and servers shutdown cleanly (without warnings).
TEST_F(RingBufferServerTest, CleanClientDrop) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);
  auto [control, ring_buffer_client] = SetupForCleanShutdownTesting();

  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
  RunLoopUntilIdle();

  // If RingBuffer client doesn't drop cleanly, RingBufferServer emits a WARNING, which will fail.
}

TEST_F(RingBufferServerTest, DriverRingBufferDropCausesCleanRingBufferServerShutdown) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);
  auto [control, ring_buffer_client] = SetupForCleanShutdownTesting();

  fake_driver->DropRingBuffer();
  RunLoopUntilIdle();

  // If RingBufferServer doesn't shutdown cleanly, it emits a WARNING, which will cause a failure.
}

TEST_F(RingBufferServerTest, DriverStreamConfigDropCausesCleanRingBufferServerShutdown) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);
  auto [control, ring_buffer_client] = SetupForCleanShutdownTesting();

  fake_driver->DropStreamConfig();
  RunLoopUntilIdle();

  // If RingBufferServer doesn't shutdown cleanly, it emits a WARNING, which will cause a failure.
}

// Validate that Control/CreateRingBuffer succeeds and returns the expected parameters.
TEST_F(RingBufferServerTest, CreateRingBufferReturnParameters) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);

  auto registry = CreateTestRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [presence, device_to_control] = adr_service_->FindDeviceByTokenId(*token_id);
  EXPECT_EQ(presence, AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(device_to_control);

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control->client()
      ->CreateRingBuffer({{
          .options = kDefaultRingBufferOptions,
          .ring_buffer_server =
              fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
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
}

// Validate that RingBuffer/SetActiveChannels succeeds and returns an expected set_time.
TEST_F(RingBufferServerTest, DriverSupportsSetActiveChannels) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);

  auto registry = CreateTestRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [presence, device_to_control] = adr_service_->FindDeviceByTokenId(*token_id);
  EXPECT_EQ(presence, AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(device_to_control);

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control->client()
      ->CreateRingBuffer({{
          .options = kDefaultRingBufferOptions,
          .ring_buffer_server =
              fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
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
      .Then([&received_callback,
             before_set_active_channels](fidl::Result<RingBuffer::SetActiveChannels>& result) {
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
  // RunLoopUntilIdle();
}

TEST_F(RingBufferServerTest, DriverDoesNotSupportSetActiveChannels) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  // Set this fake hardware to NOT support powering-down channels.
  fake_driver->set_active_channels_supported(false);
  EnableDriverAndAddDevice(fake_driver);

  auto registry = CreateTestRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [presence, added_device] = adr_service_->FindDeviceByTokenId(*token_id);
  ASSERT_EQ(presence, AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(added_device);

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control->client()
      ->CreateRingBuffer({{
          .options = kDefaultRingBufferOptions,
          .ring_buffer_server =
              fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  EXPECT_TRUE(ring_buffer_client.is_valid());
  received_callback = false;
  ring_buffer_client->SetActiveChannels({{0}}).Then(
      [&received_callback](fidl::Result<RingBuffer::SetActiveChannels>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error()) << result.error_value().FormatDescription();
        ASSERT_TRUE(result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RingBufferSetActiveChannelsError::kMethodNotSupported);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(fake_driver->active_channels_bitmask(), 0x3u);
  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
}

// Validate that RingBuffer/Start and /Stop function as expected, including start_time.
TEST_F(RingBufferServerTest, StartAndStop) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->set_active_channels_supported(false);
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);

  auto registry = CreateTestRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [presence, device_to_control] = adr_service_->FindDeviceByTokenId(*token_id);
  EXPECT_EQ(presence, AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(device_to_control);

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control->client()
      ->CreateRingBuffer({{
          .options = kDefaultRingBufferOptions,
          .ring_buffer_server =
              fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok());
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(fake_driver->is_running());

  received_callback = false;
  auto before_start = zx::clock::get_monotonic();
  ring_buffer_client->Start({}).Then(
      [&received_callback, before_start, &fake_driver](fidl::Result<RingBuffer::Start>& result) {
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
      .Then([&received_callback](fidl::Result<RingBuffer::SetActiveChannels>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_error());
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  received_callback = false;
  ring_buffer_client->Stop({}).Then(
      [&received_callback, &fake_driver](fidl::Result<RingBuffer::Stop>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        EXPECT_FALSE(fake_driver->is_running());
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
}

// Validate that RingBuffer/WatchDelayInfo notifies of the delay received during initialization.
TEST_F(RingBufferServerTest, WatchDelayInfo) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->InjectDelayUpdate(zx::nsec(987'654'321), zx::nsec(123'456'789));
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);

  auto registry = CreateTestRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [presence, device_to_control] = adr_service_->FindDeviceByTokenId(*token_id);
  EXPECT_EQ(presence, AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(device_to_control);

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control->client()
      ->CreateRingBuffer({{
          .options = kDefaultRingBufferOptions,
          .ring_buffer_server =
              fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(fake_driver->is_running());

  received_callback = false;
  ring_buffer_client->WatchDelayInfo().Then(
      [&received_callback](fidl::Result<RingBuffer::WatchDelayInfo>& result) {
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
}

// Validate that RingBuffer/WatchDelayInfo notifies of delay changes after initialization.
TEST_F(RingBufferServerTest, DynamicDelayUpdate) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);

  auto registry = CreateTestRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [presence, device_to_control] = adr_service_->FindDeviceByTokenId(*token_id);
  ASSERT_EQ(presence, AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(device_to_control);

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control->client()
      ->CreateRingBuffer({{
          .options = kDefaultRingBufferOptions,
          .ring_buffer_server =
              fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(fake_driver->is_running());

  received_callback = false;
  ring_buffer_client->WatchDelayInfo().Then(
      [&received_callback](fidl::Result<RingBuffer::WatchDelayInfo>& result) {
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
      [&received_callback](fidl::Result<RingBuffer::WatchDelayInfo>& result) {
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
}

// Validate that the RingBufferServer is destructed if the client drops the Control.
TEST_F(RingBufferServerTest, ControlClientDropCausesRingBufferDrop) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);

  auto registry = CreateTestRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [presence, device_to_control] = adr_service_->FindDeviceByTokenId(*token_id);
  EXPECT_EQ(presence, AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(device_to_control);

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control->client()
      ->CreateRingBuffer({{
          .options = kDefaultRingBufferOptions,
          .ring_buffer_server =
              fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);

  control->client() = fidl::Client<fuchsia_audio_device::Control>();
  RunLoopUntilIdle();
  EXPECT_TRUE(control->server().WaitForShutdown());
  EXPECT_EQ(RingBufferServer::count(), 0u);
}

// Validate that the RingBufferServer is destructed if the ControlServer shuts down.
TEST_F(RingBufferServerTest, ControlServerShutdownCausesRingBufferDrop) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);

  auto registry = CreateTestRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [presence, device_to_control] = adr_service_->FindDeviceByTokenId(*token_id);
  EXPECT_EQ(presence, AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(device_to_control);

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control->client()
      ->CreateRingBuffer({{
          .options = kDefaultRingBufferOptions,
          .ring_buffer_server =
              fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);

  control->server().Shutdown(ZX_ERR_PEER_CLOSED);
  RunLoopUntilIdle();
  EXPECT_TRUE(control->server().WaitForShutdown());
  EXPECT_EQ(RingBufferServer::count(), 0u);
}

}  // namespace
}  // namespace media_audio
