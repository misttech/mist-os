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
class ProviderServerCodecWarningTest : public AudioDeviceRegistryServerTestBase {};
class ProviderServerCompositeWarningTest : public AudioDeviceRegistryServerTestBase {};
class ProviderServerDaiWarningTest : public AudioDeviceRegistryServerTestBase {};
class ProviderServerStreamConfigWarningTest : public AudioDeviceRegistryServerTestBase {};

/////////////////////
// Codec tests
//
TEST_F(ProviderServerCodecWarningTest, MissingDeviceName) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeCodecOutput();
  auto received_callback = false;

  fuchsia_audio_device::ProviderAddDeviceRequest request;
  // missing .device_name
  request.device_type(fuchsia_audio_device::DeviceType::kCodec);
  request.driver_client(fuchsia_audio_device::DriverClient::WithCodec(fake_driver->Enable()));

  provider->client()
      ->AddDevice(std::move(request))
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidName)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerCodecWarningTest, EmptyDeviceName) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeCodecInput();
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "",  // empty .device_name
          .device_type = fuchsia_audio_device::DeviceType::kCodec,
          .driver_client = fuchsia_audio_device::DriverClient::WithCodec(fake_driver->Enable()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidName)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerCodecWarningTest, MissingDeviceType) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeCodecNoDirection();
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          // missing .device_type
          .driver_client = fuchsia_audio_device::DriverClient::WithCodec(fake_driver->Enable()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidType)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerCodecWarningTest, MissingDriverClient) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kCodec,
          // missing .driver_client
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidDriverClient)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerCodecWarningTest, InvalidDriverClient) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kCodec,
          .driver_client = fuchsia_audio_device::DriverClient::WithCodec(
              fidl::ClientEnd<fuchsia_hardware_audio::Codec>()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error()) << result.error_value();
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_INVALID_ARGS)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerCodecWarningTest, WrongDriverClient) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigOutput();
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kCodec,
          // StreamConfig driver_client doesn't match kCodec.
          .driver_client =
              fuchsia_audio_device::DriverClient::WithStreamConfig(fake_driver->Enable()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kWrongClientType)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

/////////////////////
// Composite tests
//
// Remove this test, once Provider/AddDevice supports the Composite driver type.
TEST_F(ProviderServerCompositeWarningTest, MissingDeviceName) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeComposite();
  auto received_callback = false;

  fuchsia_audio_device::ProviderAddDeviceRequest request;
  // missing .device_name
  request.device_type(fuchsia_audio_device::DeviceType::kComposite);
  request.driver_client(fuchsia_audio_device::DriverClient::WithComposite(fake_driver->Enable()));

  provider->client()
      ->AddDevice(std::move(request))
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidName)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerCompositeWarningTest, EmptyDeviceName) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeComposite();
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "",  // empty .device_name
          .device_type = fuchsia_audio_device::DeviceType::kComposite,
          .driver_client = fuchsia_audio_device::DriverClient::WithComposite(fake_driver->Enable()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidName)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerCompositeWarningTest, MissingDeviceType) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeComposite();
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          // missing .device_type
          .driver_client = fuchsia_audio_device::DriverClient::WithComposite(fake_driver->Enable()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidType)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerCompositeWarningTest, MissingDriverClient) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kComposite,
          // missing .driver_client
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidDriverClient)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerCompositeWarningTest, InvalidDriverClient) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kComposite,
          .driver_client = fuchsia_audio_device::DriverClient::WithComposite(
              fidl::ClientEnd<fuchsia_hardware_audio::Composite>()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error()) << result.error_value();
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_INVALID_ARGS)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerCompositeWarningTest, WrongDriverClient) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigOutput();
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kComposite,
          // StreamConfig driver_client doesn't match kComposite.
          .driver_client =
              fuchsia_audio_device::DriverClient::WithStreamConfig(fake_driver->Enable()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kWrongClientType)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

/////////////////////
// Dai tests
//
// Remove this test, once Provider/AddDevice supports the Dai driver type.
TEST_F(ProviderServerDaiWarningTest, Unsupported) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigInput();
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kDai,
          // Set a Dai device_type and driver_client -- which ADR doesn't yet support.
          .driver_client = fuchsia_audio_device::DriverClient::WithDai(
              // (as elsewhere, the zx::channel is from FakeStreamConfig, but that's irrelevant)
              fidl::ClientEnd<fuchsia_hardware_audio::Dai>(fake_driver->Enable().TakeChannel())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kWrongClientType)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

/////////////////////
// StreamConfig tests
//
TEST_F(ProviderServerStreamConfigWarningTest, MissingDeviceName) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigOutput();
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          // missing .device_name
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .driver_client =
              fuchsia_audio_device::DriverClient::WithStreamConfig(fake_driver->Enable()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidName)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerStreamConfigWarningTest, EmptyDeviceName) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigInput();
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "",  // empty .device_name
          .device_type = fuchsia_audio_device::DeviceType::kInput,
          .driver_client =
              fuchsia_audio_device::DriverClient::WithStreamConfig(fake_driver->Enable()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidName)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerStreamConfigWarningTest, MissingDeviceType) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigOutput();
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          // missing .device_type
          .driver_client =
              fuchsia_audio_device::DriverClient::WithStreamConfig(fake_driver->Enable()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidType)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerStreamConfigWarningTest, MissingDriverClient) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          // missing .driver_client
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidDriverClient)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerStreamConfigWarningTest, InvalidDriverClient) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .driver_client = fuchsia_audio_device::DriverClient::WithStreamConfig(
              fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error()) << result.error_value();
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_INVALID_ARGS)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerStreamConfigWarningTest, WrongDriverClient) {
  auto provider = CreateTestProviderServer();
  ASSERT_EQ(ProviderServer::count(), 1u);
  auto fake_driver = CreateFakeCodecOutput();
  auto received_callback = false;

  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          // Codec driver_client doesn't match kOutput.
          .driver_client = fuchsia_audio_device::DriverClient::WithCodec(fake_driver->Enable()),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kWrongClientType)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

}  // namespace
}  // namespace media_audio
