// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/registry_server.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <lib/fidl/cpp/client.h>
#include <lib/sync/cpp/completion.h>

#include <optional>

#include <gtest/gtest.h>

#include "src/media/audio/services/common/testing/test_server_and_async_client.h"
#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"

namespace media_audio {
namespace {

using Registry = fuchsia_audio_device::Registry;
using DriverClient = fuchsia_audio_device::DriverClient;

class RegistryServerTest : public AudioDeviceRegistryServerTestBase {};
class RegistryServerCodecTest : public RegistryServerTest {};
class RegistryServerStreamConfigTest : public RegistryServerTest {};

/////////////////////
// Device-less tests
//
// A client can drop their Registry connection without hang, and without WARNING being logged.
TEST_F(RegistryServerTest, CleanClientDrop) {
  auto registry = CreateTestRegistryServer();
  EXPECT_EQ(RegistryServer::count(), 1u);

  registry->client() = fidl::Client<fuchsia_audio_device::Registry>();
}

// Server can cleanly shutdown without hang, and without WARNING being logged.
TEST_F(RegistryServerTest, CleanServerShutdown) {
  auto registry = CreateTestRegistryServer();
  EXPECT_EQ(RegistryServer::count(), 1u);

  registry->server().Shutdown(ZX_ERR_PEER_CLOSED);
}

/////////////////////
// Codec tests
//
// Device already exists before the Registry connection is created.
// Client calls WatchDevicesAdded and is notified.
TEST_F(RegistryServerCodecTest, DeviceAddThenRegistryCreate) {
  auto fake_driver = CreateFakeCodecOutput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test codec name",
                                         fuchsia_audio_device::DeviceType::kCodec,
                                         DriverClient::WithCodec(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto received_callback = false;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        EXPECT_EQ(result->devices()->at(0).device_type(), fuchsia_audio_device::DeviceType::kCodec);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// Client calls WatchDevicesAdded, then add device and client is notified.
TEST_F(RegistryServerCodecTest, WatchAddsThenDeviceAdd) {
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(adr_service_->devices().size(), 0u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  auto received_callback = false;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        EXPECT_EQ(result->devices()->at(0).device_type(), fuchsia_audio_device::DeviceType::kCodec);
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);

  auto fake_driver = CreateFakeCodecInput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test codec name",
                                         fuchsia_audio_device::DeviceType::kCodec,
                                         DriverClient::WithCodec(fake_driver->Enable())));

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
}

// Add device, see it added, then client calls WatchDevicesAdded and is notified.
TEST_F(RegistryServerCodecTest, DeviceAddThenWatchAdds) {
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(adr_service_->devices().size(), 0u);
  auto fake_driver = CreateFakeCodecOutput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test codec name",
                                         fuchsia_audio_device::DeviceType::kCodec,
                                         DriverClient::WithCodec(fake_driver->Enable())));

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  auto received_callback = false;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        EXPECT_EQ(result->devices()->at(0).device_type(), fuchsia_audio_device::DeviceType::kCodec);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// Client calls WatchDeviceRemoved, then remove device, then client is notified.
TEST_F(RegistryServerCodecTest, WatchRemovesThenDeviceRemove) {
  auto fake_driver = CreateFakeCodecInput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test codec name",
                                         fuchsia_audio_device::DeviceType::kCodec,
                                         DriverClient::WithCodec(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto received_callback = false;
  std::optional<TokenId> added_device_id;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback,
       &added_device_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok() && result->devices() && result->devices()->size() == 1);
        ASSERT_TRUE(result->devices()->at(0).device_type() ==
                        fuchsia_audio_device::DeviceType::kCodec &&
                    result->devices()->at(0).token_id().has_value());
        added_device_id = *result->devices()->at(0).token_id();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(added_device_id);
  received_callback = false;
  std::optional<TokenId> removed_device_id;

  registry->client()->WatchDeviceRemoved().Then(
      [&received_callback,
       &removed_device_id](fidl::Result<Registry::WatchDeviceRemoved>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->token_id());
        removed_device_id = *result->token_id();
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(received_callback);
  EXPECT_FALSE(removed_device_id.has_value());
  fake_driver->DropCodec();

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  ASSERT_TRUE(removed_device_id.has_value());
  EXPECT_EQ(*added_device_id, *removed_device_id);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
}

// Remove device, see ADR remove it, then client calls WatchDeviceRemoved and is notified.
TEST_F(RegistryServerCodecTest, DeviceRemoveThenWatchRemoves) {
  auto fake_driver = CreateFakeCodecOutput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test codec name",
                                         fuchsia_audio_device::DeviceType::kCodec,
                                         DriverClient::WithCodec(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto received_callback = false;
  std::optional<TokenId> added_device_id;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback,
       &added_device_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok() && result->devices() && result->devices()->size() == 1);
        ASSERT_TRUE(result->devices()->at(0).device_type() ==
                        fuchsia_audio_device::DeviceType::kCodec &&
                    result->devices()->at(0).token_id().has_value());
        added_device_id = *result->devices()->at(0).token_id();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(added_device_id);
  fake_driver->DropCodec();

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 0u);
  received_callback = false;
  std::optional<TokenId> removed_device_id;

  registry->client()->WatchDeviceRemoved().Then(
      [&received_callback,
       &removed_device_id](fidl::Result<Registry::WatchDeviceRemoved>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->token_id());
        removed_device_id = *result->token_id();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  ASSERT_TRUE(removed_device_id.has_value());
  EXPECT_EQ(*added_device_id, *removed_device_id);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
}

// Create a Registry connection then add and remove device (see ADR count go up and down).
// Then when client calls WatchDevicesAdded and WatchDeviceRemoved, no notifications should occur.
TEST_F(RegistryServerCodecTest, DeviceAddRemoveThenWatches) {
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto fake_driver = CreateFakeCodecInput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test codec name",
                                         fuchsia_audio_device::DeviceType::kCodec,
                                         DriverClient::WithCodec(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  fake_driver->DropCodec();

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 0u);
  bool received_add_response = false, received_remove_response = false;

  registry->client()->WatchDevicesAdded().Then(
      [&received_add_response](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_add_response = true;
        FAIL() << "Unexpected WatchDevicesAdded response";
      });
  registry->client()->WatchDeviceRemoved().Then(
      [&received_remove_response](fidl::Result<Registry::WatchDeviceRemoved>& result) mutable {
        received_remove_response = true;
        FAIL() << "Unexpected WatchDeviceRemoved response";
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(received_add_response);
  EXPECT_FALSE(received_remove_response);
}

// Remove device, add device, WatchDevicesAdded/WatchDeviceRemoved (id's differ: should notify).
TEST_F(RegistryServerCodecTest, DeviceRemoveAddThenWatches) {
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto fake_driver = CreateFakeCodecNoDirection();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test codec name",
                                         fuchsia_audio_device::DeviceType::kCodec,
                                         DriverClient::WithCodec(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  bool received_callback = false;
  std::optional<TokenId> first_added_id;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback,
       &first_added_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok() && result->devices() && result->devices()->size() == 1);
        ASSERT_TRUE(result->devices()->at(0).device_type() ==
                        fuchsia_audio_device::DeviceType::kCodec &&
                    result->devices()->at(0).token_id().has_value());
        first_added_id = *result->devices()->at(0).token_id();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(first_added_id);
  fake_driver->DropCodec();

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 0u);
  fake_driver = CreateFakeCodecNoDirection();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test codec name",
                                         fuchsia_audio_device::DeviceType::kCodec,
                                         DriverClient::WithCodec(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  received_callback = false;
  std::optional<uint64_t> removed_device_id;

  registry->client()->WatchDeviceRemoved().Then(
      [&received_callback,
       &removed_device_id](fidl::Result<Registry::WatchDeviceRemoved>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->token_id());
        removed_device_id = *result->token_id();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(removed_device_id);
  EXPECT_EQ(*first_added_id, *removed_device_id);
  received_callback = false;
  std::optional<TokenId> second_added_id;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback,
       &second_added_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok() && result->devices() && result->devices()->size() == 1);
        ASSERT_TRUE(result->devices()->at(0).device_type() ==
                        fuchsia_audio_device::DeviceType::kCodec &&
                    result->devices()->at(0).token_id().has_value());
        second_added_id = *result->devices()->at(0).token_id();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  ASSERT_TRUE(second_added_id);
  EXPECT_NE(*first_added_id, *second_added_id);
}

// Add cases for info.signalprocessing_topologies and signalprocessing_elements (once implemented).
//
// TODO(https://fxbug.dev/323270827): implement signalprocessing for Codec (topology, gain).

// Client can open an Observer connection on an added Codec device.
TEST_F(RegistryServerCodecTest, CreateObserver) {
  auto fake_driver = CreateFakeCodecOutput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test codec name",
                                         fuchsia_audio_device::DeviceType::kCodec,
                                         DriverClient::WithCodec(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto received_callback = true;
  std::optional<TokenId> added_id;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback, &added_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok() && result->devices() && result->devices()->size() == 1);
        ASSERT_TRUE(result->devices()->at(0).device_type() ==
                        fuchsia_audio_device::DeviceType::kCodec &&
                    result->devices()->at(0).token_id().has_value());
        added_id = *result->devices()->at(0).token_id();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(added_id);
  auto [observer_client_end, observer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Observer>();
  auto observer_client = fidl::Client<fuchsia_audio_device::Observer>(
      std::move(observer_client_end), dispatcher(), observer_fidl_handler_.get());
  received_callback = false;

  registry->client()
      ->CreateObserver({{
          .token_id = *added_id,
          .observer_server =
              fidl::ServerEnd<fuchsia_audio_device::Observer>(std::move(observer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Registry::CreateObserver>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

/////////////////////
// StreamConfig tests
// Device already exists before the Registry connection is created.
TEST_F(RegistryServerStreamConfigTest, DeviceAddThenRegistryCreate) {
  auto fake_driver = CreateFakeStreamConfigOutput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test output name",
                                         fuchsia_audio_device::DeviceType::kOutput,
                                         DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto received_callback = false;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        EXPECT_EQ(result->devices()->at(0).device_type(),
                  fuchsia_audio_device::DeviceType::kOutput);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// Client calls WatchDevicesAdded, then add device and client is notified.
TEST_F(RegistryServerStreamConfigTest, WatchAddsThenDeviceAdd) {
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(adr_service_->devices().size(), 0u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  auto received_callback = false;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        EXPECT_EQ(result->devices()->at(0).device_type(), fuchsia_audio_device::DeviceType::kInput);
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);

  auto fake_driver = CreateFakeStreamConfigInput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test input name",
                                         fuchsia_audio_device::DeviceType::kInput,
                                         DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
}

// Add device, see it added, then client calls WatchDevicesAdded and is notified.
TEST_F(RegistryServerStreamConfigTest, DeviceAddThenWatchAdds) {
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(adr_service_->devices().size(), 0u);
  auto fake_driver = CreateFakeStreamConfigOutput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test output name",
                                         fuchsia_audio_device::DeviceType::kOutput,
                                         DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  auto received_callback = false;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        EXPECT_EQ(result->devices()->at(0).device_type(),
                  fuchsia_audio_device::DeviceType::kOutput);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// Client calls WatchDeviceRemoved, then remove device, then client is notified.
TEST_F(RegistryServerStreamConfigTest, WatchRemovesThenDeviceRemove) {
  auto fake_driver = CreateFakeStreamConfigInput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test input name",
                                         fuchsia_audio_device::DeviceType::kInput,
                                         DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto received_callback = false;
  std::optional<TokenId> added_device_id;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback,
       &added_device_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok() && result->devices() && result->devices()->size() == 1);
        ASSERT_TRUE(result->devices()->at(0).device_type() ==
                        fuchsia_audio_device::DeviceType::kInput &&
                    result->devices()->at(0).token_id().has_value());
        added_device_id = *result->devices()->at(0).token_id();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(added_device_id);
  received_callback = false;
  std::optional<TokenId> removed_device_id;

  registry->client()->WatchDeviceRemoved().Then(
      [&received_callback,
       &removed_device_id](fidl::Result<Registry::WatchDeviceRemoved>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->token_id());
        removed_device_id = *result->token_id();
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(received_callback);
  EXPECT_FALSE(removed_device_id.has_value());
  fake_driver->DropStreamConfig();

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  ASSERT_TRUE(removed_device_id.has_value());
  EXPECT_EQ(*added_device_id, *removed_device_id);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
}

// Remove device, see ADR remove it, then client calls WatchDeviceRemoved and is notified.
TEST_F(RegistryServerStreamConfigTest, DeviceRemoveThenWatchRemoves) {
  auto fake_driver = CreateFakeStreamConfigOutput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test output name",
                                         fuchsia_audio_device::DeviceType::kOutput,
                                         DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto received_callback = false;
  std::optional<TokenId> added_device_id;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback,
       &added_device_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok() && result->devices() && result->devices()->size() == 1);
        ASSERT_TRUE(result->devices()->at(0).device_type() ==
                        fuchsia_audio_device::DeviceType::kOutput &&
                    result->devices()->at(0).token_id().has_value());
        added_device_id = *result->devices()->at(0).token_id();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(added_device_id);
  fake_driver->DropStreamConfig();

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 0u);
  received_callback = false;
  std::optional<TokenId> removed_device_id;

  registry->client()->WatchDeviceRemoved().Then(
      [&received_callback,
       &removed_device_id](fidl::Result<Registry::WatchDeviceRemoved>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->token_id());
        removed_device_id = *result->token_id();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  ASSERT_TRUE(removed_device_id.has_value());
  EXPECT_EQ(*added_device_id, *removed_device_id);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
}

// Create a Registry connection then add and remove device (see ADR count go up and down).
// Then when client calls WatchDevicesAdded and WatchDeviceRemoved, no notifications should occur.
TEST_F(RegistryServerStreamConfigTest, DeviceAddRemoveThenWatches) {
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigInput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test input name",
                                         fuchsia_audio_device::DeviceType::kInput,
                                         DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  fake_driver->DropStreamConfig();

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 0u);
  bool received_add_response = false, received_remove_response = false;

  registry->client()->WatchDevicesAdded().Then(
      [&received_add_response](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_add_response = true;
        FAIL() << "Unexpected WatchDevicesAdded response";
      });
  registry->client()->WatchDeviceRemoved().Then(
      [&received_remove_response](fidl::Result<Registry::WatchDeviceRemoved>& result) mutable {
        received_remove_response = true;
        FAIL() << "Unexpected WatchDeviceRemoved response";
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(received_add_response);
  EXPECT_FALSE(received_remove_response);
}

// Remove device, add device, WatchDevicesAdded/WatchDeviceRemoved (id's differ: should notify).
TEST_F(RegistryServerStreamConfigTest, DeviceRemoveAddThenWatches) {
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigOutput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test output name",
                                         fuchsia_audio_device::DeviceType::kOutput,
                                         DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  bool received_callback = false;
  std::optional<TokenId> first_added_id;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback,
       &first_added_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok() && result->devices() && result->devices()->size() == 1);
        ASSERT_TRUE(result->devices()->at(0).device_type() ==
                        fuchsia_audio_device::DeviceType::kOutput &&
                    result->devices()->at(0).token_id().has_value());
        first_added_id = *result->devices()->at(0).token_id();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(first_added_id);
  fake_driver->DropStreamConfig();

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 0u);
  fake_driver = CreateFakeStreamConfigInput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test input name",
                                         fuchsia_audio_device::DeviceType::kInput,
                                         DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  received_callback = false;
  std::optional<uint64_t> removed_device_id;

  registry->client()->WatchDeviceRemoved().Then(
      [&received_callback,
       &removed_device_id](fidl::Result<Registry::WatchDeviceRemoved>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value();
        ASSERT_TRUE(result->token_id());
        removed_device_id = *result->token_id();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(removed_device_id);
  EXPECT_EQ(*first_added_id, *removed_device_id);
  received_callback = false;
  std::optional<TokenId> second_added_id;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback,
       &second_added_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok() && result->devices() && result->devices()->size() == 1);
        ASSERT_TRUE(result->devices()->at(0).device_type() ==
                        fuchsia_audio_device::DeviceType::kInput &&
                    result->devices()->at(0).token_id().has_value());
        second_added_id = *result->devices()->at(0).token_id();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  ASSERT_TRUE(second_added_id);
  EXPECT_NE(*first_added_id, *second_added_id);
}

// Add cases for info.signalprocessing_topologies and signalprocessing_elements (once implemented).
//
// TODO(https://fxbug.dev/323270827): implement signalprocessing for Codec (topology, gain).

// Client can open an Observer connection on an added Codec device.
TEST_F(RegistryServerStreamConfigTest, CreateObserver) {
  auto fake_driver = CreateFakeStreamConfigOutput();
  adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test output name",
                                         fuchsia_audio_device::DeviceType::kOutput,
                                         DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto received_callback = true;
  std::optional<TokenId> added_id;

  registry->client()->WatchDevicesAdded().Then(
      [&received_callback, &added_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok() && result->devices() && result->devices()->size() == 1);
        ASSERT_TRUE(result->devices()->at(0).device_type() ==
                        fuchsia_audio_device::DeviceType::kOutput &&
                    result->devices()->at(0).token_id().has_value());
        added_id = *result->devices()->at(0).token_id();
      });

  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);
  ASSERT_TRUE(added_id);
  auto [observer_client_end, observer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Observer>();
  auto observer_client = fidl::Client<fuchsia_audio_device::Observer>(
      std::move(observer_client_end), dispatcher(), observer_fidl_handler_.get());
  received_callback = false;

  registry->client()
      ->CreateObserver({{
          .token_id = *added_id,
          .observer_server =
              fidl::ServerEnd<fuchsia_audio_device::Observer>(std::move(observer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Registry::CreateObserver>& result) {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

}  // namespace
}  // namespace media_audio
