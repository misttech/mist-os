// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/markers.h>

#include <memory>

#include <gtest/gtest.h>

#include "src/media/audio/services/common/testing/test_server_and_async_client.h"
#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"

namespace media_audio {
namespace {

class ProviderServerTest : public AudioDeviceRegistryServerTestBase {
 protected:
  std::pair<fidl::Client<fuchsia_audio_device::Registry>, std::shared_ptr<RegistryServer>>
  CreateRegistryServer() {
    auto [client_end, server_end] = CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Registry>();
    auto server = adr_service_->CreateRegistryServer(std::move(server_end));
    auto client = fidl::Client<fuchsia_audio_device::Registry>(std::move(client_end), dispatcher());
    return std::make_pair(std::move(client), server);
  }
};

TEST_F(ProviderServerTest, AddDeviceThatOutlivesProvider) {
  auto provider_wrapper = CreateProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeDriver();
  auto stream_config_client_end =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());

  auto received_callback = false;
  provider_wrapper->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .stream_config_client = std::move(stream_config_client_end),
      }})
      .Then([&received_callback](fidl::Result<fuchsia_audio_device::Provider::AddDevice>& result) {
        EXPECT_TRUE(result.is_ok());
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  provider_wrapper.reset();
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerTest, ProviderCanOutliveDevice) {
  auto provider_wrapper = CreateProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeDriver();
  auto stream_config_client_end =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());
  auto received_callback = false;
  provider_wrapper->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .stream_config_client = std::move(stream_config_client_end),
      }})
      .Then([&received_callback](fidl::Result<fuchsia_audio_device::Provider::AddDevice>& result) {
        EXPECT_TRUE(result.is_ok());
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  fake_driver->DropStreamConfig();
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 0u);

  EXPECT_EQ(ProviderServer::count(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

// For devices added by Provider, ensure that Add-then-Watch works as expected.
TEST_F(ProviderServerTest, ProviderAddThenWatch) {
  auto provider_wrapper = CreateProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto [reg_client, reg_server] = CreateRegistryServer();
  RunLoopUntilIdle();
  EXPECT_EQ(RegistryServer::count(), 1u);

  auto fake_driver = CreateFakeDriver();
  auto stream_config_client_end =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());
  auto received_callback = false;
  provider_wrapper->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .stream_config_client = std::move(stream_config_client_end),
      }})
      .Then([&received_callback](fidl::Result<fuchsia_audio_device::Provider::AddDevice>& result) {
        EXPECT_TRUE(result.is_ok());
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  std::optional<TokenId> added_device;
  reg_client->WatchDevicesAdded().Then(
      [&added_device](
          fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_device = *result->devices()->at(0).token_id();
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(added_device);

  reg_client = fidl::Client<fuchsia_audio_device::Registry>();
  RunLoopUntilIdle();
  reg_server->WaitForShutdown(zx::sec(1));
}

// For devices added by Provider, ensure that Watch-then-Add works as expected.
TEST_F(ProviderServerTest, WatchThenProviderAdd) {
  auto [reg_client, reg_server] = CreateRegistryServer();
  RunLoopUntilIdle();
  EXPECT_EQ(RegistryServer::count(), 1u);

  std::optional<TokenId> added_device;
  reg_client->WatchDevicesAdded().Then(
      [&added_device](
          fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_device = *result->devices()->at(0).token_id();
      });
  RunLoopUntilIdle();
  EXPECT_FALSE(added_device);

  auto provider_wrapper = CreateProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);
  RunLoopUntilIdle();
  EXPECT_FALSE(added_device);

  auto fake_driver = CreateFakeDriver();
  auto stream_config_client_end =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());
  auto received_callback = false;
  provider_wrapper->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .stream_config_client = std::move(stream_config_client_end),
      }})
      .Then([&received_callback](fidl::Result<fuchsia_audio_device::Provider::AddDevice>& result) {
        EXPECT_TRUE(result.is_ok());
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  EXPECT_TRUE(added_device);

  reg_client = fidl::Client<fuchsia_audio_device::Registry>();
  RunLoopUntilIdle();
  reg_server->WaitForShutdown(zx::sec(1));
}

}  // namespace
}  // namespace media_audio
