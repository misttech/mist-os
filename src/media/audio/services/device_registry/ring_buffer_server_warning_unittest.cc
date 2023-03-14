// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/markers.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <lib/fidl/cpp/wire/internal/transport_channel.h>
#include <lib/syslog/cpp/macros.h>
#include <lib/zx/clock.h>

#include <gmock/gmock.h>
#include <gtest/gtest.h>

#include "src/media/audio/services/common/testing/test_server_and_async_client.h"
#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/ring_buffer_server.h"

namespace media_audio {
namespace {

using ::testing::Optional;
using Control = fuchsia_audio_device::Control;
using RingBuffer = fuchsia_audio_device::RingBuffer;

class RingBufferServerWarningTest
    : public AudioDeviceRegistryServerTestBase,
      public fidl::AsyncEventHandler<fuchsia_audio_device::RingBuffer> {
 protected:
  const fuchsia_audio_device::RingBufferOptions kDefaultRingBufferOptions = {{
      .format = fuchsia_audio::Format{{
          .sample_type = fuchsia_audio::SampleType::kInt16,
          .channel_count = 2,
          .frames_per_second = 48000,
      }},
      .ring_buffer_min_bytes = 2000,
  }};

  std::unique_ptr<FakeAudioDriver> CreateFakeDriverWithDefaults();
  void EnableDriverAndAddDevice(const std::unique_ptr<FakeAudioDriver>& fake_driver);

  std::optional<TokenId> WaitForAddedDeviceTokenId(
      fidl::Client<fuchsia_audio_device::Registry>& reg_client);

  std::pair<fidl::Client<fuchsia_audio_device::RingBuffer>,
            fidl::ServerEnd<fuchsia_audio_device::RingBuffer>>
  CreateRingBufferClient();
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
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
}

std::optional<TokenId> RingBufferServerWarningTest::WaitForAddedDeviceTokenId(
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
RingBufferServerWarningTest::CreateRingBufferClient() {
  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
      fidl::ClientEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_client_end)),
      dispatcher(), this);
  return std::make_pair(std::move(ring_buffer_client), std::move(ring_buffer_server_end));
}

TEST_F(RingBufferServerWarningTest, SetActiveChannelsMissingChannelBitmask) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);

  auto registry = CreateTestRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);

  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*token_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
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
  // No `channel_bitmask` value is included in this call.
  ring_buffer_client->SetActiveChannels({}).Then(
      [&received_callback](fidl::Result<RingBuffer::SetActiveChannels>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error()) << result.error_value().FormatDescription();
        ASSERT_TRUE(result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RingBufferSetActiveChannelsError::kInvalidChannelBitmask);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(fake_driver->active_channels_bitmask(), 0x3u);
  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
}

TEST_F(RingBufferServerWarningTest, SetActiveChannelsBadChannelBitmask) {
  auto fake_driver = CreateFakeDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);
  EnableDriverAndAddDevice(fake_driver);

  auto registry = CreateTestRegistryServer();
  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*token_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
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
  ring_buffer_client
      ->SetActiveChannels({{
          0xFFFF,  // This channel bitmask includes values outside the total number of channels.
      }})
      .Then([&received_callback](fidl::Result<RingBuffer::SetActiveChannels>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error()) << result.error_value().FormatDescription();
        ASSERT_TRUE(result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RingBufferSetActiveChannelsError::kChannelOutOfRange);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(fake_driver->active_channels_bitmask(), 0x3u);
  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
}

// Test Start when already Started
TEST_F(RingBufferServerWarningTest, StartWhileStarted) {
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
  ring_buffer_client->Start({}).Then([&received_callback](fidl::Result<RingBuffer::Start>& result) {
    received_callback = true;
    EXPECT_TRUE(result.is_error());
    EXPECT_TRUE(result.error_value().is_domain_error());
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::RingBufferStartError::kAlreadyStarted);
  });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
}

// Test Stop when not yet Started.
TEST_F(RingBufferServerWarningTest, StopBeforeStarted) {
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
  ring_buffer_client->Stop({}).Then([&received_callback](fidl::Result<RingBuffer::Stop>& result) {
    received_callback = true;
    EXPECT_TRUE(result.is_error());
    EXPECT_TRUE(result.error_value().is_domain_error());
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::RingBufferStopError::kAlreadyStopped);
  });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
}

// Test Stop after Start-then-Stop.
TEST_F(RingBufferServerWarningTest, StopAfterStopped) {
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
  ring_buffer_client->Stop({}).Then(
      [&received_callback, &fake_driver](fidl::Result<RingBuffer::Stop>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        EXPECT_FALSE(fake_driver->is_running());
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  received_callback = false;
  ring_buffer_client->Stop({}).Then([&received_callback](fidl::Result<RingBuffer::Stop>& result) {
    received_callback = true;
    EXPECT_TRUE(result.is_error());
    EXPECT_TRUE(result.error_value().is_domain_error());
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::RingBufferStopError::kAlreadyStopped);
  });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
}

// Test WatchDelayInfo when already watching - should fail with kWatchAlreadyPending.
TEST_F(RingBufferServerWarningTest, WatchDelayInfoWhileAlreadyWatching) {
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
        ASSERT_TRUE(result->delay_info()->internal_delay());
        ASSERT_FALSE(result->delay_info()->external_delay());
        EXPECT_THAT(result->delay_info()->internal_delay(), Optional(0u));
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  received_callback = false;
  ring_buffer_client->WatchDelayInfo().Then(
      [&received_callback](fidl::Result<RingBuffer::WatchDelayInfo>& result) {
        ADD_FAILURE() << "Unexpected WatchDelayInfo response received: "
                      << (result.is_ok() ? "OK" : result.error_value().FormatDescription());
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_FALSE(received_callback);

  received_callback = false;
  ring_buffer_client->WatchDelayInfo().Then(
      [&received_callback](fidl::Result<RingBuffer::WatchDelayInfo>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error());
        ASSERT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RingBufferWatchDelayInfoError::kWatchAlreadyPending);
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
}

// TODO(fxbug/dev:117199): When Health can change post-initialization, test when Healthy device
// becomes Unhealthy right before (1) SetActiveChannels, (2) Start, (3) Stop, (4) WatchDelayInfo.
// In all four cases, expect Observers/Control/RingBuffer to drop and Registry/WatchDeviceRemoved.

}  // namespace
}  // namespace media_audio
