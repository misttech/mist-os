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
#include "src/media/audio/services/device_registry/common_unittest.h"
#include "src/media/audio/services/device_registry/ring_buffer_server.h"
#include "src/media/audio/services/device_registry/testing/fake_composite.h"
#include "src/media/audio/services/device_registry/testing/fake_composite_ring_buffer.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"

namespace media_audio {
namespace {

using ::testing::Optional;
using Control = fuchsia_audio_device::Control;
using RingBuffer = fuchsia_audio_device::RingBuffer;
using DriverClient = fuchsia_audio_device::DriverClient;

class RingBufferServerWarningTest
    : public AudioDeviceRegistryServerTestBase,
      public fidl::AsyncEventHandler<fuchsia_audio_device::RingBuffer> {
 protected:
  static fuchsia_audio_device::RingBufferOptions DefaultRingBufferOptions() {
    return {{
        .format = fuchsia_audio::Format{{
            .sample_type = fuchsia_audio::SampleType::kInt16,
            .channel_count = 2,
            .frames_per_second = 48000,
        }},
        .ring_buffer_min_bytes = 2000,
    }};
  }

  std::optional<TokenId> WaitForAddedDeviceTokenId(
      fidl::Client<fuchsia_audio_device::Registry>& reg_client) {
    std::optional<TokenId> added_device_id;
    reg_client->WatchDevicesAdded().Then(
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

  std::pair<fidl::Client<fuchsia_audio_device::RingBuffer>,
            fidl::ServerEnd<fuchsia_audio_device::RingBuffer>>
  CreateRingBufferClient() {
    auto [ring_buffer_client_end, ring_buffer_server_end] =
        CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
    auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
        std::move(ring_buffer_client_end), dispatcher(), this);
    return std::make_pair(std::move(ring_buffer_client), std::move(ring_buffer_server_end));
  }
};

class RingBufferServerCompositeWarningTest : public RingBufferServerWarningTest {
 protected:
  std::shared_ptr<Device> EnableDriverAndAddDevice(
      const std::shared_ptr<FakeComposite>& fake_driver) {
    auto device = Device::Create(adr_service_, dispatcher(), "Test composite name",
                                 fuchsia_audio_device::DeviceType::kComposite,
                                 DriverClient::WithComposite(fake_driver->Enable()));
    adr_service_->AddDevice(device);

    RunLoopUntilIdle();
    return device;
  }

  void PrepareRingBufferClientForNegativeTesting(const std::shared_ptr<FakeComposite>& fake_driver,
                                                 ElementId element_id) {
    fake_driver->ReserveRingBufferSize(element_id, 8192);
    auto device = EnableDriverAndAddDevice(fake_driver);
    auto format = SafeRingBufferFormatFromElementRingBufferFormatSets(
        element_id, device->ring_buffer_format_sets());

    auto registry = CreateTestRegistryServer();
    auto token_id = WaitForAddedDeviceTokenId(registry->client());
    ASSERT_TRUE(token_id);

    auto [status, added_device] = adr_service_->FindDeviceByTokenId(*token_id);
    ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
    auto control_ = CreateTestControlServer(added_device);

    auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
    bool received_callback = false;
    control_->client()
        ->CreateRingBuffer({{
            element_id,
            fuchsia_audio_device::RingBufferOptions{
                {.format = format, .ring_buffer_min_bytes = 2000}},
            std::move(ring_buffer_server_end),
        }})
        .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
          EXPECT_TRUE(result.is_ok()) << result.error_value();
          received_callback = true;
        });

    RunLoopUntilIdle();
    ASSERT_TRUE(received_callback);
    ring_buffer_client_ = std::move(ring_buffer_client);
    EXPECT_TRUE(ring_buffer_client_.is_valid());
    device_ = std::move(added_device);
  }

  fidl::Client<fuchsia_audio_device::RingBuffer>& ring_buffer_client() {
    return ring_buffer_client_;
  }

 private:
  std::unique_ptr<TestServerAndAsyncClient<media_audio::ControlServer, fidl::Client>> control_;
  std::shared_ptr<Device> device_;
  fidl::Client<fuchsia_audio_device::RingBuffer> ring_buffer_client_;
};

TEST_F(RingBufferServerCompositeWarningTest, SetActiveChannelsMissingChannelBitmask) {
  auto fake_driver = CreateFakeComposite();
  auto element_id = FakeComposite::kMaxRingBufferElementId;
  fake_driver->ReserveRingBufferSize(element_id, 8192);
  fake_driver->EnableActiveChannelsSupport(element_id);
  auto device = EnableDriverAndAddDevice(fake_driver);
  auto format = SafeRingBufferFormatFromElementRingBufferFormatSets(
      element_id, device->ring_buffer_format_sets());
  auto registry = CreateTestRegistryServer();

  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*token_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto control_ = CreateTestControlServer(added_device);
  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;

  control_->client()
      ->CreateRingBuffer({{
          element_id,
          fuchsia_audio_device::RingBufferOptions{
              {.format = format, .ring_buffer_min_bytes = 2000}},
          std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  EXPECT_TRUE(ring_buffer_client.is_valid());
  ASSERT_EQ(fake_driver->active_channels_bitmask(element_id), (1u << *format.channel_count()) - 1u);
  received_callback = false;

  ring_buffer_client
      ->SetActiveChannels({
          // No `channel_bitmask` value is included in this call.
      })
      .Then([&received_callback](fidl::Result<RingBuffer::SetActiveChannels>& result) {
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RingBufferSetActiveChannelsError::kInvalidChannelBitmask)
            << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  // This should be entirely unchanged.
  EXPECT_EQ(fake_driver->active_channels_bitmask(element_id), (1u << *format.channel_count()) - 1u);
}

TEST_F(RingBufferServerCompositeWarningTest, SetActiveChannelsBadChannelBitmask) {
  auto fake_driver = CreateFakeComposite();
  auto element_id = FakeComposite::kMaxRingBufferElementId;
  fake_driver->ReserveRingBufferSize(element_id, 8192);
  fake_driver->EnableActiveChannelsSupport(element_id);
  auto device = EnableDriverAndAddDevice(fake_driver);
  auto format = SafeRingBufferFormatFromElementRingBufferFormatSets(
      element_id, device->ring_buffer_format_sets());
  auto registry = CreateTestRegistryServer();

  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*token_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto control_ = CreateTestControlServer(added_device);
  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;

  control_->client()
      ->CreateRingBuffer({{
          element_id,
          fuchsia_audio_device::RingBufferOptions{
              {.format = format, .ring_buffer_min_bytes = 2000}},
          std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  EXPECT_TRUE(ring_buffer_client.is_valid());
  ASSERT_EQ(fake_driver->active_channels_bitmask(element_id), (1u << *format.channel_count()) - 1u);
  received_callback = false;

  ring_buffer_client
      ->SetActiveChannels({{
          0xFFFF,  // This channel bitmask includes values outside the total number of channels.
      }})
      .Then([&received_callback](fidl::Result<RingBuffer::SetActiveChannels>& result) {
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RingBufferSetActiveChannelsError::kChannelOutOfRange)
            << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(fake_driver->active_channels_bitmask(element_id), (1u << *format.channel_count()) - 1u);
}

// Test calling SetActiveChannels, before the previous SetActiveChannels has completed.
TEST_F(RingBufferServerCompositeWarningTest, SetActiveChannelsWhilePending) {
  auto fake_driver = CreateFakeComposite();
  auto element_id = FakeComposite::kMaxRingBufferElementId;
  fake_driver->ReserveRingBufferSize(element_id, 8192);
  fake_driver->EnableActiveChannelsSupport(element_id);
  auto device = EnableDriverAndAddDevice(fake_driver);
  auto format = SafeRingBufferFormatFromElementRingBufferFormatSets(
      element_id, device->ring_buffer_format_sets());
  auto registry = CreateTestRegistryServer();

  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*token_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto control_ = CreateTestControlServer(added_device);
  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;

  control_->client()
      ->CreateRingBuffer({{
          element_id,
          fuchsia_audio_device::RingBufferOptions{
              {.format = format, .ring_buffer_min_bytes = 2000}},
          std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  EXPECT_TRUE(ring_buffer_client.is_valid());
  ASSERT_EQ(fake_driver->active_channels_bitmask(element_id), (1u << *format.channel_count()) - 1u);
  bool received_callback_1 = false, received_callback_2 = false;

  ring_buffer_client->SetActiveChannels({{1}}).Then(
      [&received_callback_1](fidl::Result<RingBuffer::SetActiveChannels>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback_1 = true;
      });
  ring_buffer_client->SetActiveChannels({{0}}).Then(
      [&received_callback_2](fidl::Result<RingBuffer::SetActiveChannels>& result) {
        ASSERT_TRUE(result.is_error()) << result.error_value();
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RingBufferSetActiveChannelsError::kAlreadyPending)
            << result.error_value();
        received_callback_2 = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback_1 && received_callback_2);
  EXPECT_EQ(fake_driver->active_channels_bitmask(element_id), 0x1u);
  EXPECT_EQ(RingBufferServer::count(), 1u);
}

// Test Start-Start, when the second Start is called before the first Start completes.
TEST_F(RingBufferServerCompositeWarningTest, StartWhilePending) {
  auto fake_driver = CreateFakeComposite();
  auto element_id = FakeComposite::kMaxRingBufferElementId;
  fake_driver->ReserveRingBufferSize(element_id, 8192);
  fake_driver->EnableActiveChannelsSupport(element_id);
  auto device = EnableDriverAndAddDevice(fake_driver);
  auto format = SafeRingBufferFormatFromElementRingBufferFormatSets(
      element_id, device->ring_buffer_format_sets());
  auto registry = CreateTestRegistryServer();

  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*token_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto control_ = CreateTestControlServer(added_device);
  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;

  control_->client()
      ->CreateRingBuffer({{
          element_id,
          fuchsia_audio_device::RingBufferOptions{
              {.format = format, .ring_buffer_min_bytes = 2000}},
          std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  EXPECT_TRUE(ring_buffer_client.is_valid());
  ASSERT_EQ(fake_driver->active_channels_bitmask(element_id), (1u << *format.channel_count()) - 1u);
  bool received_callback_1 = false, received_callback_2 = false;

  ring_buffer_client->Start({}).Then(
      [&received_callback_1, &fake_driver, element_id](fidl::Result<RingBuffer::Start>& result) {
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        EXPECT_TRUE(fake_driver->started(element_id));
        received_callback_1 = true;
      });
  ring_buffer_client->Start({}).Then(
      [&received_callback_2, &fake_driver, element_id](fidl::Result<RingBuffer::Start>& result) {
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RingBufferStartError::kAlreadyPending)
            << result.error_value();
        EXPECT_TRUE(fake_driver->started(element_id));
        received_callback_2 = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback_1 && received_callback_2);
  EXPECT_TRUE(fake_driver->started(element_id));
  EXPECT_EQ(RingBufferServer::count(), 1u);
}

// Test Start-Start, when the second Start occurs after the first has successfully completed.
TEST_F(RingBufferServerCompositeWarningTest, StartWhileStarted) {
  auto fake_driver = CreateFakeComposite();
  auto element_id = FakeComposite::kMaxRingBufferElementId;
  fake_driver->ReserveRingBufferSize(element_id, 8192);
  fake_driver->EnableActiveChannelsSupport(element_id);
  auto device = EnableDriverAndAddDevice(fake_driver);
  auto format = SafeRingBufferFormatFromElementRingBufferFormatSets(
      element_id, device->ring_buffer_format_sets());
  auto registry = CreateTestRegistryServer();

  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*token_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto control_ = CreateTestControlServer(added_device);
  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;

  control_->client()
      ->CreateRingBuffer({{
          element_id,
          fuchsia_audio_device::RingBufferOptions{
              {.format = format, .ring_buffer_min_bytes = 2000}},
          std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  EXPECT_TRUE(ring_buffer_client.is_valid());
  ASSERT_EQ(fake_driver->active_channels_bitmask(element_id), (1u << *format.channel_count()) - 1u);
  received_callback = false;
  auto before_start = zx::clock::get_monotonic();

  ring_buffer_client->Start({}).Then([&received_callback, before_start, &fake_driver,
                                      element_id](fidl::Result<RingBuffer::Start>& result) {
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result->start_time());
    EXPECT_EQ(*result->start_time(), fake_driver->mono_start_time(element_id).get());
    EXPECT_GT(*result->start_time(), before_start.get());
    EXPECT_TRUE(fake_driver->started(element_id));
    received_callback = true;
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  received_callback = false;
  ring_buffer_client->Start({}).Then([&received_callback](fidl::Result<RingBuffer::Start>& result) {
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::RingBufferStartError::kAlreadyStarted)
        << result.error_value();
    received_callback = true;
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(RingBufferServer::count(), 1u);
}

// Test Stop when not yet Started.
TEST_F(RingBufferServerCompositeWarningTest, StopBeforeStarted) {
  auto fake_driver = CreateFakeComposite();
  auto element_id = FakeComposite::kMaxRingBufferElementId;
  fake_driver->ReserveRingBufferSize(element_id, 8192);
  fake_driver->EnableActiveChannelsSupport(element_id);
  auto device = EnableDriverAndAddDevice(fake_driver);
  auto format = SafeRingBufferFormatFromElementRingBufferFormatSets(
      element_id, device->ring_buffer_format_sets());
  auto registry = CreateTestRegistryServer();

  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*token_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto control_ = CreateTestControlServer(added_device);
  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;

  control_->client()
      ->CreateRingBuffer({{
          element_id,
          fuchsia_audio_device::RingBufferOptions{
              {.format = format, .ring_buffer_min_bytes = 2000}},
          std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(ring_buffer_client.is_valid());
  ASSERT_EQ(fake_driver->active_channels_bitmask(element_id), (1u << *format.channel_count()) - 1u);
  received_callback = false;

  ring_buffer_client->Stop({}).Then([&received_callback](fidl::Result<RingBuffer::Stop>& result) {
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::RingBufferStopError::kAlreadyStopped)
        << result.error_value();
    received_callback = true;
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// Test Start-Stop-Stop, when the second Stop is called before the first one completes.
TEST_F(RingBufferServerCompositeWarningTest, StopWhilePending) {
  auto fake_driver = CreateFakeComposite();
  auto element_id = FakeComposite::kMaxRingBufferElementId;
  fake_driver->ReserveRingBufferSize(element_id, 8192);
  fake_driver->EnableActiveChannelsSupport(element_id);
  auto device = EnableDriverAndAddDevice(fake_driver);
  auto format = SafeRingBufferFormatFromElementRingBufferFormatSets(
      element_id, device->ring_buffer_format_sets());
  auto registry = CreateTestRegistryServer();

  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*token_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto control_ = CreateTestControlServer(added_device);
  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;

  control_->client()
      ->CreateRingBuffer({{
          element_id,
          fuchsia_audio_device::RingBufferOptions{
              {.format = format, .ring_buffer_min_bytes = 2000}},
          std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(ring_buffer_client.is_valid());
  ASSERT_EQ(fake_driver->active_channels_bitmask(element_id), (1u << *format.channel_count()) - 1u);
  received_callback = false;
  auto before_start = zx::clock::get_monotonic();

  ring_buffer_client->Start({}).Then([&received_callback, before_start, &fake_driver,
                                      element_id](fidl::Result<RingBuffer::Start>& result) {
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result->start_time());
    EXPECT_EQ(*result->start_time(), fake_driver->mono_start_time(element_id).get());
    EXPECT_GT(*result->start_time(), before_start.get());
    EXPECT_TRUE(fake_driver->started(element_id));
    received_callback = true;
  });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  EXPECT_TRUE(ring_buffer_client.is_valid());
  ASSERT_EQ(fake_driver->active_channels_bitmask(element_id), (1u << *format.channel_count()) - 1u);
  bool received_callback_1 = false, received_callback_2 = false;

  ring_buffer_client->Stop({}).Then(
      [&received_callback_1, &fake_driver, element_id](fidl::Result<RingBuffer::Stop>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        EXPECT_FALSE(fake_driver->started(element_id));
        received_callback_1 = true;
      });
  ring_buffer_client->Stop({}).Then([&received_callback_2](fidl::Result<RingBuffer::Stop>& result) {
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::RingBufferStopError::kAlreadyPending)
        << result.error_value();
    received_callback_2 = true;
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback_1 && received_callback_2);
  EXPECT_EQ(RingBufferServer::count(), 1u);
}

// Test Start-Stop-Stop, when the first Stop successfully completed before the second is called.
TEST_F(RingBufferServerCompositeWarningTest, StopAfterStopped) {
  auto fake_driver = CreateFakeComposite();
  auto element_id = FakeComposite::kMaxRingBufferElementId;
  fake_driver->ReserveRingBufferSize(element_id, 8192);
  fake_driver->EnableActiveChannelsSupport(element_id);
  auto device = EnableDriverAndAddDevice(fake_driver);
  auto format = SafeRingBufferFormatFromElementRingBufferFormatSets(
      element_id, device->ring_buffer_format_sets());
  auto registry = CreateTestRegistryServer();

  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*token_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto control_ = CreateTestControlServer(added_device);
  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;

  control_->client()
      ->CreateRingBuffer({{
          element_id,
          fuchsia_audio_device::RingBufferOptions{
              {.format = format, .ring_buffer_min_bytes = 2000}},
          std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(ring_buffer_client.is_valid());
  ASSERT_EQ(fake_driver->active_channels_bitmask(element_id), (1u << *format.channel_count()) - 1u);
  received_callback = false;
  auto before_start = zx::clock::get_monotonic();

  ring_buffer_client->Start({}).Then([&received_callback, before_start, &fake_driver,
                                      element_id](fidl::Result<RingBuffer::Start>& result) {
    ASSERT_TRUE(result.is_ok()) << result.error_value();
    ASSERT_TRUE(result->start_time());
    EXPECT_EQ(*result->start_time(), fake_driver->mono_start_time(element_id).get());
    EXPECT_GT(*result->start_time(), before_start.get());
    EXPECT_TRUE(fake_driver->started(element_id));
    received_callback = true;
  });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(ring_buffer_client.is_valid());
  ASSERT_EQ(fake_driver->active_channels_bitmask(element_id), (1u << *format.channel_count()) - 1u);
  received_callback = false;

  ring_buffer_client->Stop({}).Then(
      [&received_callback, &fake_driver, element_id](fidl::Result<RingBuffer::Stop>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        EXPECT_FALSE(fake_driver->started(element_id));
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  received_callback = false;

  ring_buffer_client->Stop({}).Then([&received_callback](fidl::Result<RingBuffer::Stop>& result) {
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::RingBufferStopError::kAlreadyStopped)
        << result.error_value();
    received_callback = true;
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// Test WatchDelayInfo when already watching - should fail with kAlreadyPending.
TEST_F(RingBufferServerCompositeWarningTest, WatchDelayInfoWhilePending) {
  auto fake_driver = CreateFakeComposite();
  auto element_id = FakeComposite::kMaxRingBufferElementId;
  fake_driver->ReserveRingBufferSize(element_id, 8192);
  fake_driver->EnableActiveChannelsSupport(element_id);
  auto device = EnableDriverAndAddDevice(fake_driver);
  auto format = SafeRingBufferFormatFromElementRingBufferFormatSets(
      element_id, device->ring_buffer_format_sets());
  auto registry = CreateTestRegistryServer();

  auto token_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(token_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*token_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto control_ = CreateTestControlServer(added_device);
  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;

  control_->client()
      ->CreateRingBuffer({{
          element_id,
          fuchsia_audio_device::RingBufferOptions{
              {.format = format, .ring_buffer_min_bytes = 2000}},
          std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  EXPECT_TRUE(ring_buffer_client.is_valid());
  ASSERT_EQ(fake_driver->active_channels_bitmask(element_id), (1u << *format.channel_count()) - 1u);
  received_callback = false;

  ring_buffer_client->WatchDelayInfo().Then(
      [&received_callback](fidl::Result<RingBuffer::WatchDelayInfo>& result) {
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->delay_info());
        ASSERT_TRUE(result->delay_info()->internal_delay());
        EXPECT_FALSE(result->delay_info()->external_delay());
        EXPECT_EQ(*result->delay_info()->internal_delay(),
                  FakeCompositeRingBuffer::kDefaultInternalDelay->get());
        received_callback = true;
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
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RingBufferWatchDelayInfoError::kAlreadyPending)
            << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

class RingBufferServerStreamConfigWarningTest : public RingBufferServerWarningTest {
 protected:
  void EnableDriverAndAddDevice(const std::shared_ptr<FakeStreamConfig>& fake_driver) {
    adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test input name",
                                           fuchsia_audio_device::DeviceType::kInput,
                                           DriverClient::WithStreamConfig(fake_driver->Enable())));

    RunLoopUntilIdle();
  }
};

TEST_F(RingBufferServerStreamConfigWarningTest, SetActiveChannelsMissingChannelBitmask) {
  auto fake_driver = CreateFakeStreamConfigInput();
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
          .options = DefaultRingBufferOptions(),
          .ring_buffer_server = std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  EXPECT_TRUE(ring_buffer_client.is_valid());
  ASSERT_EQ(fake_driver->active_channels_bitmask(), 0x3u);
  received_callback = false;

  ring_buffer_client
      ->SetActiveChannels({
          // No `channel_bitmask` value is included in this call.
      })
      .Then([&received_callback](fidl::Result<RingBuffer::SetActiveChannels>& result) {
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RingBufferSetActiveChannelsError::kInvalidChannelBitmask)
            << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(fake_driver->active_channels_bitmask(), 0x3u);
}

TEST_F(RingBufferServerStreamConfigWarningTest, SetActiveChannelsBadChannelBitmask) {
  auto fake_driver = CreateFakeStreamConfigInput();
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
          .options = DefaultRingBufferOptions(),
          .ring_buffer_server = std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_EQ(fake_driver->active_channels_bitmask(), 0x3u);
  EXPECT_TRUE(ring_buffer_client.is_valid());
  received_callback = false;

  ring_buffer_client
      ->SetActiveChannels({{
          0xFFFF,  // This channel bitmask includes values outside the total number of channels.
      }})
      .Then([&received_callback](fidl::Result<RingBuffer::SetActiveChannels>& result) {
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RingBufferSetActiveChannelsError::kChannelOutOfRange)
            << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(fake_driver->active_channels_bitmask(), 0x3u);
}

// Test calling SetActiveChannels, before the previous SetActiveChannels has completed.
TEST_F(RingBufferServerStreamConfigWarningTest, SetActiveChannelsWhilePending) {
  auto fake_driver = CreateFakeStreamConfigInput();
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
          .options = DefaultRingBufferOptions(),
          .ring_buffer_server = std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_EQ(fake_driver->active_channels_bitmask(), 0x3u);
  ASSERT_EQ(RingBufferServer::count(), 1u);
  EXPECT_TRUE(ring_buffer_client.is_valid());
  bool received_callback_1 = false, received_callback_2 = false;

  ring_buffer_client->SetActiveChannels({{1}}).Then(
      [&received_callback_1](fidl::Result<RingBuffer::SetActiveChannels>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback_1 = true;
      });
  ring_buffer_client->SetActiveChannels({{0}}).Then(
      [&received_callback_2](fidl::Result<RingBuffer::SetActiveChannels>& result) {
        ASSERT_TRUE(result.is_error()) << result.error_value();
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RingBufferSetActiveChannelsError::kAlreadyPending)
            << result.error_value();
        received_callback_2 = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback_1 && received_callback_2);
  EXPECT_EQ(fake_driver->active_channels_bitmask(), 0x1u);
  EXPECT_EQ(RingBufferServer::count(), 1u);
}

// Test Start-Start, when the second Start is called before the first Start completes.
TEST_F(RingBufferServerStreamConfigWarningTest, StartWhilePending) {
  auto fake_driver = CreateFakeStreamConfigInput();
  fake_driver->set_active_channels_supported(false);
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
          .options = DefaultRingBufferOptions(),
          .ring_buffer_server = std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_FALSE(fake_driver->started());
  ASSERT_EQ(RingBufferServer::count(), 1u);
  bool received_callback_1 = false, received_callback_2 = false;

  ring_buffer_client->Start({}).Then(
      [&received_callback_1, &fake_driver](fidl::Result<RingBuffer::Start>& result) {
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        EXPECT_TRUE(fake_driver->started());
        received_callback_1 = true;
      });
  ring_buffer_client->Start({}).Then(
      [&received_callback_2, &fake_driver](fidl::Result<RingBuffer::Start>& result) {
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RingBufferStartError::kAlreadyPending)
            << result.error_value();
        EXPECT_TRUE(fake_driver->started());
        received_callback_2 = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback_1 && received_callback_2);
  EXPECT_TRUE(fake_driver->started());
  EXPECT_EQ(RingBufferServer::count(), 1u);
}

// Test Start-Start, when the second Start occurs after the first has successfully completed.
TEST_F(RingBufferServerStreamConfigWarningTest, StartWhileStarted) {
  auto fake_driver = CreateFakeStreamConfigInput();
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
          .options = DefaultRingBufferOptions(),
          .ring_buffer_server = std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(fake_driver->started());
  EXPECT_EQ(RingBufferServer::count(), 1u);
  received_callback = false;
  auto before_start = zx::clock::get_monotonic();

  ring_buffer_client->Start({}).Then(
      [&received_callback, before_start, &fake_driver](fidl::Result<RingBuffer::Start>& result) {
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->start_time());
        EXPECT_EQ(*result->start_time(), fake_driver->mono_start_time().get());
        EXPECT_GT(*result->start_time(), before_start.get());
        EXPECT_TRUE(fake_driver->started());
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  received_callback = false;

  ring_buffer_client->Start({}).Then([&received_callback](fidl::Result<RingBuffer::Start>& result) {
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::RingBufferStartError::kAlreadyStarted)
        << result.error_value();
    received_callback = true;
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(RingBufferServer::count(), 1u);
}

// Test Stop when not yet Started.
TEST_F(RingBufferServerStreamConfigWarningTest, StopBeforeStarted) {
  auto fake_driver = CreateFakeStreamConfigInput();
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
          .options = DefaultRingBufferOptions(),
          .ring_buffer_server = std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(fake_driver->started());
  received_callback = false;

  ring_buffer_client->Stop({}).Then([&received_callback](fidl::Result<RingBuffer::Stop>& result) {
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::RingBufferStopError::kAlreadyStopped)
        << result.error_value();
    received_callback = true;
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// Test Start-Stop-Stop, when the second Stop is called before the first one completes.
TEST_F(RingBufferServerStreamConfigWarningTest, StopWhilePending) {
  auto fake_driver = CreateFakeStreamConfigInput();
  fake_driver->set_active_channels_supported(false);
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
          .options = DefaultRingBufferOptions(),
          .ring_buffer_server = std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_FALSE(fake_driver->started());
  received_callback = false;

  ring_buffer_client->Start({}).Then(
      [&received_callback, &fake_driver](fidl::Result<RingBuffer::Start>& result) {
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(fake_driver->started());
        received_callback = true;
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_EQ(RingBufferServer::count(), 1u);
  bool received_callback_1 = false, received_callback_2 = false;

  ring_buffer_client->Stop({}).Then(
      [&received_callback_1, &fake_driver](fidl::Result<RingBuffer::Stop>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        EXPECT_FALSE(fake_driver->started());
        received_callback_1 = true;
      });
  ring_buffer_client->Stop({}).Then([&received_callback_2](fidl::Result<RingBuffer::Stop>& result) {
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::RingBufferStopError::kAlreadyPending)
        << result.error_value();
    received_callback_2 = true;
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback_1 && received_callback_2);
  EXPECT_EQ(RingBufferServer::count(), 1u);
}

// Test Start-Stop-Stop, when the first Stop successfully completed before the second is called.
TEST_F(RingBufferServerStreamConfigWarningTest, StopAfterStopped) {
  auto fake_driver = CreateFakeStreamConfigInput();
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
          .options = DefaultRingBufferOptions(),
          .ring_buffer_server = std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(fake_driver->started());
  received_callback = false;
  auto before_start = zx::clock::get_monotonic();
  ring_buffer_client->Start({}).Then(
      [&received_callback, before_start, &fake_driver](fidl::Result<RingBuffer::Start>& result) {
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->start_time());
        EXPECT_EQ(*result->start_time(), fake_driver->mono_start_time().get());
        EXPECT_GT(*result->start_time(), before_start.get());
        EXPECT_TRUE(fake_driver->started());
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  received_callback = false;
  ring_buffer_client->Stop({}).Then(
      [&received_callback, &fake_driver](fidl::Result<RingBuffer::Stop>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        EXPECT_FALSE(fake_driver->started());
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  received_callback = false;
  ring_buffer_client->Stop({}).Then([&received_callback](fidl::Result<RingBuffer::Stop>& result) {
    ASSERT_TRUE(result.is_error());
    ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_audio_device::RingBufferStopError::kAlreadyStopped)
        << result.error_value();
    received_callback = true;
  });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// Test WatchDelayInfo when already watching - should fail with kAlreadyPending.
TEST_F(RingBufferServerStreamConfigWarningTest, WatchDelayInfoWhilePending) {
  auto fake_driver = CreateFakeStreamConfigInput();
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
          .options = DefaultRingBufferOptions(),
          .ring_buffer_server = std::move(ring_buffer_server_end),
      }})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(fake_driver->started());
  received_callback = false;

  ring_buffer_client->WatchDelayInfo().Then(
      [&received_callback](fidl::Result<RingBuffer::WatchDelayInfo>& result) {
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->delay_info());
        ASSERT_TRUE(result->delay_info()->internal_delay());
        EXPECT_FALSE(result->delay_info()->external_delay());
        EXPECT_EQ(*result->delay_info()->internal_delay(), 0u);
        received_callback = true;
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
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RingBufferWatchDelayInfoError::kAlreadyPending)
            << result.error_value();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test: device becomes
//   Unhealthy right before (1) SetActiveChannels, (2) Start, (3) Stop, (4) WatchDelayInfo. In all
//   four cases, expect Observers/Control/RingBuffer to drop + Registry/WatchDeviceRemoved notif.

}  // namespace
}  // namespace media_audio
