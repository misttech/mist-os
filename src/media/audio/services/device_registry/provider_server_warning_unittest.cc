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

// These tests rely upon a single, already-created Provider.
class ProviderServerWarningTest : public AudioDeviceRegistryServerTestBase {
  //  protected:
  // void SetUp() override {
  //   AudioDeviceRegistryServerTestBase::SetUp();

  // auto provider_wrapper = std::make_unique<TestServerAndNaturalAsyncClient<ProviderServer>>(
  //     test_loop(), server_thread_, adr_service_);
  // EXPECT_EQ(ProviderServer::count(), 1u);
  //   auto endpoints = CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Provider>();
  //   provider_server_ = adr_service_->CreateProviderServer(std::move(endpoints.second));
  //   provider_client_ = fidl::Client(std::move(endpoints.first), dispatcher());

  //   EXPECT_EQ(ProviderServer::count(), 1u);
  // }

  // // If we explicitly shutdown the client first, we avoid dispatcher_shutdown errors.
  // void TearDown() override {
  //   provider_client_ = fidl::Client<fuchsia_audio_device::Provider>();
  //   RunLoopUntilIdle();
  //   EXPECT_TRUE(provider_server_->WaitForShutdown(zx::sec(1)));
  // }

  // std::shared_ptr<ProviderServer> provider_server_;
  // fidl::Client<fuchsia_audio_device::Provider> provider_client_;
};

TEST_F(ProviderServerWarningTest, MissingDeviceName) {
  auto provider_wrapper = std::make_unique<TestServerAndNaturalAsyncClient<ProviderServer>>(
      test_loop(), server_thread_, adr_service_);
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeDriver();
  auto stream_config_client_end =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());

  auto received_callback = false;
  provider_wrapper->client()
      ->AddDevice({{
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .stream_config_client = std::move(stream_config_client_end),
      }})
      .Then([&received_callback](fidl::Result<fuchsia_audio_device::Provider::AddDevice>& result) {
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
  auto provider_wrapper = std::make_unique<TestServerAndNaturalAsyncClient<ProviderServer>>(
      test_loop(), server_thread_, adr_service_);
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeDriver();
  auto stream_config_client_end =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());

  auto received_callback = false;
  provider_wrapper->client()
      ->AddDevice({{
          .device_name = "",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .stream_config_client = std::move(stream_config_client_end),
      }})
      .Then([&received_callback](fidl::Result<fuchsia_audio_device::Provider::AddDevice>& result) {
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
  auto provider_wrapper = std::make_unique<TestServerAndNaturalAsyncClient<ProviderServer>>(
      test_loop(), server_thread_, adr_service_);
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeDriver();
  auto stream_config_client_end =
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable());

  auto received_callback = false;
  provider_wrapper->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .stream_config_client = std::move(stream_config_client_end),
      }})
      .Then([&received_callback](fidl::Result<fuchsia_audio_device::Provider::AddDevice>& result) {
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
  auto provider_wrapper = std::make_unique<TestServerAndNaturalAsyncClient<ProviderServer>>(
      test_loop(), server_thread_, adr_service_);
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto received_callback = false;
  provider_wrapper->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
      }})
      .Then([&received_callback](fidl::Result<fuchsia_audio_device::Provider::AddDevice>& result) {
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
  auto provider_wrapper = std::make_unique<TestServerAndNaturalAsyncClient<ProviderServer>>(
      test_loop(), server_thread_, adr_service_);
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto received_callback = false;
  provider_wrapper->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .stream_config_client = fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(),
      }})
      .Then([&received_callback](fidl::Result<fuchsia_audio_device::Provider::AddDevice>& result) {
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
