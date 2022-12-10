// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/registry_server.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <lib/sync/cpp/completion.h>

#include <optional>

#include <gtest/gtest.h>

#include "fidl/fuchsia.audio.device/cpp/markers.h"
#include "lib/fidl/cpp/client.h"
#include "src/media/audio/services/common/testing/test_server_and_async_client.h"
#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"

namespace media_audio {
namespace {

class RegistryServerTest : public AudioDeviceRegistryServerTestBase {
 protected:
  std::pair<fidl::Client<fuchsia_audio_device::Registry>, std::shared_ptr<RegistryServer>>
  CreateRegistryServer() {
    auto [client_end, server_end] = CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Registry>();
    auto server = adr_service_->CreateRegistryServer(std::move(server_end));
    auto client = fidl::Client<fuchsia_audio_device::Registry>(std::move(client_end), dispatcher());
    return std::make_pair(std::move(client), server);
  }
};

// Device already exists before the Registry connection is created.
TEST_F(RegistryServerTest, DeviceAddThenRegistryCreate) {
  auto fake_driver = CreateFakeDriver();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);

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
  EXPECT_TRUE(added_device);

  reg_client = fidl::Client<fuchsia_audio_device::Registry>();
  // Let the loop clear out after dropping the client, so the server has time to destruct.
  // Otherwise BaseFidlServer treats it as an unexpected shutdown.
  RunLoopUntilIdle();
  reg_server->WaitForShutdown(zx::sec(1));
}

// WatchDevicesAdded, then add device.
TEST_F(RegistryServerTest, WatchAddsThenDeviceAdd) {
  auto [reg_client, reg_server] = CreateRegistryServer();
  RunLoopUntilIdle();
  EXPECT_EQ(RegistryServer::count(), 1u);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
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
  EXPECT_FALSE(added_device);

  auto fake_driver = CreateFakeDriver();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_TRUE(added_device);

  reg_client = fidl::Client<fuchsia_audio_device::Registry>();
  RunLoopUntilIdle();
  reg_server->WaitForShutdown(zx::sec(1));
}

// Add device then WatchDevicesAdded.
TEST_F(RegistryServerTest, DeviceAddThenWatchAdds) {
  auto [reg_client, reg_server] = CreateRegistryServer();
  RunLoopUntilIdle();
  EXPECT_EQ(RegistryServer::count(), 1u);
  EXPECT_EQ(adr_service_->devices().size(), 0u);

  auto fake_driver = CreateFakeDriver();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);

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

// WatchDeviceRemoved, then remove device.
TEST_F(RegistryServerTest, WatchRemovesThenDeviceRemove) {
  auto fake_driver = CreateFakeDriver();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);

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
  ASSERT_TRUE(added_device);

  std::optional<uint64_t> removed_device;
  reg_client->WatchDeviceRemoved().Then(
      [&removed_device](
          fidl::Result<fuchsia_audio_device::Registry::WatchDeviceRemoved>& result) mutable {
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->token_id());
        removed_device = *result->token_id();
      });
  RunLoopUntilIdle();
  EXPECT_FALSE(removed_device);

  fake_driver->DropStreamConfig();
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  ASSERT_TRUE(removed_device);
  EXPECT_EQ(*added_device, *removed_device);

  reg_client = fidl::Client<fuchsia_audio_device::Registry>();
  RunLoopUntilIdle();
  reg_server->WaitForShutdown(zx::sec(1));
}

// Remove device, then WatchDeviceRemoved.
TEST_F(RegistryServerTest, DeviceRemoveThenWatchRemoves) {
  auto fake_driver = CreateFakeDriver();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);

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
  ASSERT_TRUE(added_device);

  fake_driver->DropStreamConfig();
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 0u);

  std::optional<uint64_t> removed_device;
  reg_client->WatchDeviceRemoved().Then(
      [&removed_device](
          fidl::Result<fuchsia_audio_device::Registry::WatchDeviceRemoved>& result) mutable {
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->token_id());
        removed_device = *result->token_id();
      });
  RunLoopUntilIdle();
  ASSERT_TRUE(removed_device);
  EXPECT_EQ(*added_device, *removed_device);

  reg_client = fidl::Client<fuchsia_audio_device::Registry>();
  RunLoopUntilIdle();
  reg_server->WaitForShutdown(zx::sec(1));
}

// Add device, remove device, then WatchDevicesAdded/WatchDeviceRemoved (should not notify).
TEST_F(RegistryServerTest, DeviceAddRemoveThenWatches) {
  auto [reg_client, reg_server] = CreateRegistryServer();
  RunLoopUntilIdle();
  EXPECT_EQ(RegistryServer::count(), 1u);

  auto fake_driver = CreateFakeDriver();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);

  fake_driver->DropStreamConfig();
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 0u);

  bool received_add_response = false, received_remove_response = false;
  reg_client->WatchDevicesAdded().Then(
      [&received_add_response](
          fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
        received_add_response = true;
        FAIL() << "Unexpected WatchDevicesAdded response";
      });
  reg_client->WatchDeviceRemoved().Then(
      [&received_remove_response](
          fidl::Result<fuchsia_audio_device::Registry::WatchDeviceRemoved>& result) mutable {
        received_remove_response = true;
        FAIL() << "Unexpected WatchDeviceRemoved response";
      });
  RunLoopUntilIdle();
  EXPECT_FALSE(received_add_response);
  EXPECT_FALSE(received_remove_response);

  reg_client = fidl::Client<fuchsia_audio_device::Registry>();
  RunLoopUntilIdle();
  reg_server->WaitForShutdown(zx::sec(1));
}

// Remove device, add device, WatchDevicesAdded/WatchDeviceRemoved (id's differ: should notify).
TEST_F(RegistryServerTest, DeviceRemoveAddThenWatches) {
  auto [reg_client, reg_server] = CreateRegistryServer();
  RunLoopUntilIdle();
  EXPECT_EQ(RegistryServer::count(), 1u);

  auto fake_driver = CreateFakeDriver();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);

  std::optional<TokenId> first_id;
  reg_client->WatchDevicesAdded().Then(
      [&first_id](fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        first_id = *result->devices()->at(0).token_id();
      });
  RunLoopUntilIdle();
  ASSERT_TRUE(first_id);

  fake_driver->DropStreamConfig();
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 0u);

  fake_driver = CreateFakeDriver();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);

  std::optional<uint64_t> removed_device;
  reg_client->WatchDeviceRemoved().Then(
      [&removed_device](
          fidl::Result<fuchsia_audio_device::Registry::WatchDeviceRemoved>& result) mutable {
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->token_id());
        removed_device = *result->token_id();
      });
  RunLoopUntilIdle();
  ASSERT_TRUE(removed_device);
  EXPECT_EQ(*first_id, *removed_device);

  std::optional<TokenId> second_id;
  reg_client->WatchDevicesAdded().Then(
      [&second_id](
          fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        second_id = *result->devices()->at(0).token_id();
      });
  RunLoopUntilIdle();
  ASSERT_TRUE(second_id);
  EXPECT_NE(*first_id, *second_id);

  reg_client = fidl::Client<fuchsia_audio_device::Registry>();
  RunLoopUntilIdle();
  reg_server->WaitForShutdown(zx::sec(1));
}

TEST_F(RegistryServerTest, CreateObserver) {
  auto fake_driver = CreateFakeDriver();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);

  auto [reg_client, reg_server] = CreateRegistryServer();
  RunLoopUntilIdle();
  EXPECT_EQ(RegistryServer::count(), 1u);

  std::optional<TokenId> id;
  reg_client->WatchDevicesAdded().Then(
      [&id](fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        id = *result->devices()->at(0).token_id();
      });
  RunLoopUntilIdle();
  ASSERT_TRUE(id);

  auto [observer_client_end, observer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Observer>();
  auto client = fidl::Client<fuchsia_audio_device::Observer>(
      fidl::ClientEnd<fuchsia_audio_device::Observer>(std::move(observer_client_end)),
      dispatcher());
  bool received_callback = false;
  reg_client
      ->CreateObserver({{
          .token_id = *id,
          .observer_server =
              fidl::ServerEnd<fuchsia_audio_device::Observer>(std::move(observer_server_end)),
      }})
      .Then([&received_callback](
                fidl::Result<fuchsia_audio_device::Registry::CreateObserver>& result) {
        EXPECT_TRUE(result.is_ok());
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  reg_client = fidl::Client<fuchsia_audio_device::Registry>();
  RunLoopUntilIdle();
  reg_server->WaitForShutdown(zx::sec(1));
}

}  // namespace
}  // namespace media_audio
