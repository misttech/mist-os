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

using Provider = fuchsia_audio_device::Provider;
using Registry = fuchsia_audio_device::Registry;

class ProviderServerTest : public AudioDeviceRegistryServerTestBase {};

TEST_F(ProviderServerTest, CleanClientDrop) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  provider->client() = fidl::Client<Provider>();

  // No WARNING logging should occur.
}

TEST_F(ProviderServerTest, CleanServerShutdown) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  provider->server().Shutdown(ZX_ERR_PEER_CLOSED);

  // No WARNING logging should occur.
}

////////////////////////////////////////////
// Validate AddDevice(StreamConfig)
//
TEST_F(ProviderServerTest, AddStreamConfigThatOutlivesProvider) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeStreamConfigOutput();

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test output",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .driver_client = fuchsia_audio_device::DriverClient::WithStreamConfig(
              fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  provider->client() = fidl::Client<Provider>();
  RunLoopUntilIdle();
  EXPECT_TRUE(provider->server().WaitForShutdown(zx::sec(1)));
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerTest, ProviderCanOutliveStreamConfig) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeStreamConfigInput();
  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test input",
          .device_type = fuchsia_audio_device::DeviceType::kInput,
          .driver_client = fuchsia_audio_device::DriverClient::WithStreamConfig(
              fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
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

// For StreamConfigs added by Provider, ensure that Add-then-Watch works as expected.
TEST_F(ProviderServerTest, ProviderAddStreamConfigThenWatch) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto registry_wrapper = CreateTestRegistryServer();
  EXPECT_EQ(RegistryServer::count(), 1u);

  auto fake_driver = CreateFakeStreamConfigOutput();
  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test output",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .driver_client = fuchsia_audio_device::DriverClient::WithStreamConfig(
              fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  std::optional<TokenId> added_device;
  registry_wrapper->client()->WatchDevicesAdded().Then(
      [&added_device](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_device = *result->devices()->at(0).token_id();
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(added_device);
}

// For StreamConfigs added by Provider, ensure that Watch-then-Add works as expected.
TEST_F(ProviderServerTest, WatchThenProviderAddStreamConfig) {
  auto registry_wrapper = CreateTestRegistryServer();
  EXPECT_EQ(RegistryServer::count(), 1u);

  std::optional<TokenId> added_device;
  registry_wrapper->client()->WatchDevicesAdded().Then(
      [&added_device](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_device = *result->devices()->at(0).token_id();
      });
  RunLoopUntilIdle();
  EXPECT_FALSE(added_device);

  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);
  RunLoopUntilIdle();
  EXPECT_FALSE(added_device);

  auto fake_driver = CreateFakeStreamConfigOutput();
  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test output",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .driver_client = fuchsia_audio_device::DriverClient::WithStreamConfig(
              fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  EXPECT_TRUE(added_device);
}

////////////////////////////////////////////
// Validate AddDevice(Codec)
//
TEST_F(ProviderServerTest, AddCodecThatOutlivesProvider) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeCodecOutput();

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test codec",
          .device_type = fuchsia_audio_device::DeviceType::kCodec,
          .driver_client = fuchsia_audio_device::DriverClient::WithCodec(
              fidl::ClientEnd<fuchsia_hardware_audio::Codec>(fake_driver->Enable())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  provider->client() = fidl::Client<Provider>();
  RunLoopUntilIdle();
  EXPECT_TRUE(provider->server().WaitForShutdown(zx::sec(1)));
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerTest, ProviderCanOutliveCodec) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeCodecInput();
  fake_driver->set_is_input(true);
  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test codec",
          .device_type = fuchsia_audio_device::DeviceType::kCodec,
          .driver_client = fuchsia_audio_device::DriverClient::WithCodec(
              fidl::ClientEnd<fuchsia_hardware_audio::Codec>(fake_driver->Enable())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  fake_driver->DropCodec();
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 0u);

  EXPECT_EQ(ProviderServer::count(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

// For Codecs added by Provider, ensure that Add-then-Watch works as expected.
TEST_F(ProviderServerTest, ProviderAddCodecThenWatch) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto registry_wrapper = CreateTestRegistryServer();
  EXPECT_EQ(RegistryServer::count(), 1u);

  auto fake_driver = CreateFakeCodecNoDirection();
  fake_driver->set_is_input(std::nullopt);
  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test codec",
          .device_type = fuchsia_audio_device::DeviceType::kCodec,
          .driver_client = fuchsia_audio_device::DriverClient::WithCodec(
              fidl::ClientEnd<fuchsia_hardware_audio::Codec>(fake_driver->Enable())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  std::optional<TokenId> added_device;
  registry_wrapper->client()->WatchDevicesAdded().Then(
      [&added_device](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_device = *result->devices()->at(0).token_id();
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(added_device);
}

// For Codecs added by Provider, ensure that Watch-then-Add works as expected.
TEST_F(ProviderServerTest, WatchThenProviderAddCodec) {
  auto registry_wrapper = CreateTestRegistryServer();
  EXPECT_EQ(RegistryServer::count(), 1u);

  std::optional<TokenId> added_device;
  registry_wrapper->client()->WatchDevicesAdded().Then(
      [&added_device](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_device = *result->devices()->at(0).token_id();
      });
  RunLoopUntilIdle();
  EXPECT_FALSE(added_device);

  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);
  RunLoopUntilIdle();
  EXPECT_FALSE(added_device);

  auto fake_driver = CreateFakeCodecOutput();
  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test codec",
          .device_type = fuchsia_audio_device::DeviceType::kCodec,
          .driver_client = fuchsia_audio_device::DriverClient::WithCodec(
              fidl::ClientEnd<fuchsia_hardware_audio::Codec>(fake_driver->Enable())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 1u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  EXPECT_TRUE(added_device);
}

////////////////////////////////////////////
// Validate AddDevice(Composite)  (upcoming)
//
// TEST_F(ProviderServerTest, AddCompositeThatOutlivesProvider) {}
// TEST_F(ProviderServerTest, ProviderCanOutliveComposite) {}
// TEST_F(ProviderServerTest, ProviderAddCompositeThenWatch) {}
// TEST_F(ProviderServerTest, WatchThenProviderAddComposite){}

////////////////////////////////////////////
// Validate AddDevice(Dai)  (upcoming)
//
// TEST_F(ProviderServerTest, AddDaiThatOutlivesProvider) {}
// TEST_F(ProviderServerTest, ProviderCanOutliveDai) {}
// TEST_F(ProviderServerTest, ProviderAddDaiThenWatch) {}
// TEST_F(ProviderServerTest, WatchThenProviderAddDai){}

}  // namespace
}  // namespace media_audio
