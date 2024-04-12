// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/control_creator_server.h"

#include <fidl/fuchsia.audio.device/cpp/common_types.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/common/testing/test_server_and_async_client.h"
#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"

namespace media_audio {
namespace {

class ControlCreatorServerTest : public AudioDeviceRegistryServerTestBase {};
class ControlCreatorServerCodecTest : public ControlCreatorServerTest {};
class ControlCreatorServerStreamConfigTest : public ControlCreatorServerTest {};

/////////////////////
// Device-less tests
//
// Verify that the ControlCreator client can be dropped cleanly without generating a WARNING.
TEST_F(ControlCreatorServerTest, CleanClientDrop) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);

  (void)control_creator->client().UnbindMaybeGetEndpoint();
}

// Verify that the ControlCreator server can shutdown cleanly without generating a WARNING.
TEST_F(ControlCreatorServerTest, CleanServerShutdown) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);

  control_creator->server().Shutdown(ZX_ERR_PEER_CLOSED);
}

/////////////////////
// Codec tests
//
// Validate the ControlCreator/CreateControl method for Codec devices.
TEST_F(ControlCreatorServerCodecTest, CreateControl) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto fake_driver = CreateFakeCodecOutput();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test codec name", fuchsia_audio_device::DeviceType::kCodec,
      fuchsia_audio_device::DriverClient::WithCodec(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  auto control_endpoints = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client = fidl::Client<fuchsia_audio_device::Control>(
      std::move(control_endpoints.client), dispatcher());
  auto received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = (*adr_service_->devices().begin())->token_id(),
          .control_server = std::move(control_endpoints.server),
      }})
      .Then([&received_callback](
                fidl::Result<fuchsia_audio_device::ControlCreator::Create>& result) mutable {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

/////////////////////
// StreamConfig tests
//
// Validate the ControlCreator/CreateControl method for StreamConfig devices.
TEST_F(ControlCreatorServerStreamConfigTest, CreateControl) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);
  auto fake_driver = CreateFakeStreamConfigOutput();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fuchsia_audio_device::DriverClient::WithStreamConfig(fake_driver->Enable())));

  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  auto control_endpoints = fidl::Endpoints<fuchsia_audio_device::Control>::Create();
  auto control_client = fidl::Client<fuchsia_audio_device::Control>(
      std::move(control_endpoints.client), dispatcher());
  auto received_callback = false;

  control_creator->client()
      ->Create({{
          .token_id = (*adr_service_->devices().begin())->token_id(),
          .control_server = std::move(control_endpoints.server),
      }})
      .Then([&received_callback](
                fidl::Result<fuchsia_audio_device::ControlCreator::Create>& result) mutable {
        received_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ControlServer::count(), 1u);
}

}  // namespace
}  // namespace media_audio
