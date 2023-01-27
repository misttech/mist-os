// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/provider_server.h"

namespace media_audio {
namespace {

using Provider = fuchsia_audio_device::Provider;

// These tests rely upon a single, already-created Provider.
class ProviderServerWarningTest : public AudioDeviceRegistryServerTestBase {};

TEST_F(ProviderServerWarningTest, MissingDeviceName) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeDriver();
  auto stream_config_client_end =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .stream_config_client = std::move(stream_config_client_end),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidName);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerWarningTest, EmptyDeviceName) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeDriver();
  auto stream_config_client_end =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .stream_config_client = std::move(stream_config_client_end),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidName);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerWarningTest, MissingDeviceType) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeDriver();
  auto stream_config_client_end =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .stream_config_client = std::move(stream_config_client_end),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidType);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerWarningTest, MissingStreamConfig) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidStreamConfig);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerWarningTest, InvalidStreamConfig) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .stream_config_client = fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error());
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_INVALID_ARGS);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

}  // namespace
}  // namespace media_audio
