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

///////////////////////////////////
// Validate AddDevice(Codec)
//
TEST_F(ProviderServerWarningTest, MissingDeviceNameForCodec) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeCodecOutput();

  auto received_callback = false;
  fuchsia_audio_device::ProviderAddDeviceRequest request;
  // missing .device_name
  request.device_type(fuchsia_audio_device::DeviceType::kCodec);
  request.driver_client(fuchsia_audio_device::DriverClient::WithCodec(
      fidl::ClientEnd<fuchsia_hardware_audio::Codec>(fake_driver->Enable())));
  provider->client()
      ->AddDevice(std::move(request))
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidName)
            << result.error_value().FormatDescription();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerWarningTest, EmptyDeviceNameForCodec) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeCodecInput();
  fake_driver->set_is_input(true);

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "",
          .device_type = fuchsia_audio_device::DeviceType::kCodec,
          .driver_client = fuchsia_audio_device::DriverClient::WithCodec(
              fidl::ClientEnd<fuchsia_hardware_audio::Codec>(fake_driver->Enable())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidName)
            << result.error_value().FormatDescription();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerWarningTest, MissingDeviceTypeForCodec) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeCodecNoDirection();
  fake_driver->set_is_input(std::nullopt);

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test codec",
          // missing .device_type
          .driver_client = fuchsia_audio_device::DriverClient::WithCodec(
              fidl::ClientEnd<fuchsia_hardware_audio::Codec>(fake_driver->Enable())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidType)
            << result.error_value().FormatDescription();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerWarningTest, MissingDriverClientForCodec) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test codec",                              //
          .device_type = fuchsia_audio_device::DeviceType::kCodec,  //
                                                                    // missing .driver_client
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidDriverClient)
            << result.error_value().FormatDescription();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerWarningTest, InvalidDriverClientForCodec) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

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
        ASSERT_TRUE(result.error_value().is_framework_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_INVALID_ARGS)
            << result.error_value().FormatDescription();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerWarningTest, WrongDriverClientForCodec) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeCodecOutput();

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test codec",
          .device_type = fuchsia_audio_device::DeviceType::kCodec,
          // StreamConfig driver_client doesn't match kCodec.
          .driver_client = fuchsia_audio_device::DriverClient::WithStreamConfig(
              fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kWrongClientType)
            << result.error_value().FormatDescription();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

///////////////////////////////////
// Validate AddDevice(Composite)
//
// Remove this test, once Provider/AddDevice supports the Composite driver type.
TEST_F(ProviderServerWarningTest, UnsupportedCompositeType) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeStreamConfigOutput();

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kComposite,
          // Set a Composite device_type and driver_client -- which ADR doesn't yet support.
          .driver_client = fuchsia_audio_device::DriverClient::WithComposite(
              // (as elsewhere, the zx::channel is from FakeStreamConfig, but that's irrelevant)
              fidl::ClientEnd<fuchsia_hardware_audio::Composite>(fake_driver->Enable())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kWrongClientType)
            << result.error_value().FormatDescription();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}
// TEST_F(ProviderServerWarningTest, MissingDeviceNameForComposite) {}
// TEST_F(ProviderServerWarningTest, EmptyDeviceNameForComposite) {}
// TEST_F(ProviderServerWarningTest, MissingDeviceTypeForComposite) {}
// TEST_F(ProviderServerWarningTest, MissingDriverClientForComposite) {}
// TEST_F(ProviderServerWarningTest, InvalidDriverClientForComposite) {}
// TEST_F(ProviderServerWarningTest, WrongDriverClientForComposite) {}

///////////////////////////////////
// Validate AddDevice(Dai)
//
// Remove this test, once Provider/AddDevice supports the Dai driver type.
TEST_F(ProviderServerWarningTest, UnsupportedDaiType) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeStreamConfigInput();

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          .device_type = fuchsia_audio_device::DeviceType::kDai,
          // Set a Dai device_type and driver_client -- which ADR doesn't yet support.
          .driver_client = fuchsia_audio_device::DriverClient::WithDai(
              // (as elsewhere, the zx::channel is from FakeStreamConfig, but that's irrelevant)
              fidl::ClientEnd<fuchsia_hardware_audio::Dai>(fake_driver->Enable())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kWrongClientType)
            << result.error_value().FormatDescription();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}
// TEST_F(ProviderServerWarningTest, MissingDeviceNameForDai) {}
// TEST_F(ProviderServerWarningTest, EmptyDeviceNameForDai) {}
// TEST_F(ProviderServerWarningTest, MissingDeviceTypeForDai) {}
// TEST_F(ProviderServerWarningTest, MissingDriverClientForDai) {}
// TEST_F(ProviderServerWarningTest, InvalidDriverClientForDai) {}
// TEST_F(ProviderServerWarningTest, WrongDriverClientForDai) {}

///////////////////////////////////
// Validate AddDevice(StreamConfig)
//
TEST_F(ProviderServerWarningTest, MissingDeviceNameForStreamConfig) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeStreamConfigOutput();

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          // missing .device_name
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          .driver_client = fuchsia_audio_device::DriverClient::WithStreamConfig(
              fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidName)
            << result.error_value().FormatDescription();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerWarningTest, EmptyDeviceNameForStreamConfig) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeStreamConfigInput();

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "",
          .device_type = fuchsia_audio_device::DeviceType::kInput,
          .driver_client = fuchsia_audio_device::DriverClient::WithStreamConfig(
              fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidName)
            << result.error_value().FormatDescription();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerWarningTest, MissingDeviceTypeForStreamConfig) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeStreamConfigOutput();

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test device name",
          // missing .device_type
          .driver_client = fuchsia_audio_device::DriverClient::WithStreamConfig(
              fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidType)
            << result.error_value().FormatDescription();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerWarningTest, MissingDriverClientForStreamConfig) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test output", .device_type = fuchsia_audio_device::DeviceType::kOutput,
          // missing .driver_client
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kInvalidDriverClient)
            << result.error_value().FormatDescription();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerWarningTest, InvalidDriverClientForStreamConfig) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

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
        ASSERT_TRUE(result.error_value().is_framework_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_INVALID_ARGS)
            << result.error_value().FormatDescription();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

TEST_F(ProviderServerWarningTest, WrongDriverClientForStreamConfig) {
  auto provider = CreateTestProviderServer();
  EXPECT_EQ(ProviderServer::count(), 1u);

  auto fake_driver = CreateFakeCodecOutput();

  auto received_callback = false;
  provider->client()
      ->AddDevice({{
          .device_name = "Test output",
          .device_type = fuchsia_audio_device::DeviceType::kOutput,
          // Codec driver_client doesn't match kOutput.
          .driver_client = fuchsia_audio_device::DriverClient::WithCodec(
              fidl::ClientEnd<fuchsia_hardware_audio::Codec>(fake_driver->Enable())),
      }})
      .Then([&received_callback](fidl::Result<Provider::AddDevice>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().FormatDescription();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ProviderAddDeviceError::kWrongClientType)
            << result.error_value().FormatDescription();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(adr_service_->devices().size(), 0u);
  EXPECT_EQ(adr_service_->unhealthy_devices().size(), 0u);
}

}  // namespace
}  // namespace media_audio
