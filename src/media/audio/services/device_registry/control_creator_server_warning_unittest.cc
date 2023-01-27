// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/common_types.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/common/testing/test_server_and_async_client.h"
#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/control_creator_server.h"

namespace media_audio {
namespace {

using ControlCreator = fuchsia_audio_device::ControlCreator;
using Registry = fuchsia_audio_device::Registry;

class ControlCreatorServerWarningTest : public AudioDeviceRegistryServerTestBase {};

TEST_F(ControlCreatorServerWarningTest, MissingId) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);

  zx::channel server_end, client_end;
  ASSERT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      fidl::ClientEnd<fuchsia_audio_device::Control>(std::move(client_end)), dispatcher(),
      control_fidl_handler_.get());
  auto received_callback = false;
  control_creator->client()
      ->Create({{
          // Missing token_id
          .control_server = fidl::ServerEnd<fuchsia_audio_device::Control>(std::move(server_end)),
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreatorError::kInvalidTokenId);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

TEST_F(ControlCreatorServerWarningTest, BadId) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);

  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto fake_driver = CreateFakeDriver();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  std::optional<TokenId> added_device_id;
  registry->client()->WatchDevicesAdded().Then(
      [&added_device_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_device_id = *result->devices()->at(0).token_id();
      });
  RunLoopUntilIdle();
  ASSERT_TRUE(added_device_id);

  zx::channel server_end, client_end;
  ASSERT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      fidl::ClientEnd<fuchsia_audio_device::Control>(std::move(client_end)), dispatcher(),
      control_fidl_handler_.get());
  auto received_callback = false;
  control_creator->client()
      ->Create({{
          .token_id = *added_device_id - 1,  // Bad token_id
          .control_server = fidl::ServerEnd<fuchsia_audio_device::Control>(std::move(server_end)),
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreatorError::kDeviceNotFound);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

TEST_F(ControlCreatorServerWarningTest, MissingServerEnd) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);

  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto fake_driver = CreateFakeDriver();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  std::optional<TokenId> added_device_id;
  registry->client()->WatchDevicesAdded().Then(
      [&added_device_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_device_id = *result->devices()->at(0).token_id();
      });
  RunLoopUntilIdle();
  ASSERT_TRUE(added_device_id);

  zx::channel server_end, client_end;
  ASSERT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      fidl::ClientEnd<fuchsia_audio_device::Control>(std::move(client_end)), dispatcher(),
      control_fidl_handler_.get());
  auto received_callback = false;
  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          // Missing server_end
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreatorError::kInvalidControl);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

TEST_F(ControlCreatorServerWarningTest, BadServerEnd) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);

  auto fake_driver = CreateFakeDriver();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  std::optional<TokenId> added_device_id;
  {
    auto registry = CreateTestRegistryServer();
    ASSERT_EQ(RegistryServer::count(), 1u);

    registry->client()->WatchDevicesAdded().Then(
        [&added_device_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
          ASSERT_TRUE(result.is_ok());
          ASSERT_TRUE(result->devices());
          ASSERT_EQ(result->devices()->size(), 1u);
          ASSERT_TRUE(result->devices()->at(0).token_id());
          added_device_id = *result->devices()->at(0).token_id();
        });
    RunLoopUntilIdle();
  }
  ASSERT_TRUE(added_device_id);

  zx::channel server_end, client_end;
  ASSERT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
  auto control_client_unused = fidl::Client<fuchsia_audio_device::Control>(
      fidl::ClientEnd<fuchsia_audio_device::Control>(std::move(client_end)), dispatcher(),
      control_fidl_handler_.get());
  auto received_callback = false;
  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          .control_server = fidl::ServerEnd<fuchsia_audio_device::Control>(),  // Bad server_end
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error());
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_INVALID_ARGS);
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

TEST_F(ControlCreatorServerWarningTest, IdAlreadyControlled) {
  auto control_creator = CreateTestControlCreatorServer();
  ASSERT_EQ(ControlCreatorServer::count(), 1u);

  auto fake_driver = CreateFakeDriver();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  std::optional<TokenId> added_device_id;
  {
    auto registry = CreateTestRegistryServer();
    ASSERT_EQ(RegistryServer::count(), 1u);

    registry->client()->WatchDevicesAdded().Then(
        [&added_device_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
          ASSERT_TRUE(result.is_ok());
          ASSERT_TRUE(result->devices());
          ASSERT_EQ(result->devices()->size(), 1u);
          ASSERT_TRUE(result->devices()->at(0).token_id());
          added_device_id = *result->devices()->at(0).token_id();
        });
    RunLoopUntilIdle();
  }
  ASSERT_TRUE(added_device_id);

  zx::channel server_end1, client_end1;
  ASSERT_EQ(ZX_OK, zx::channel::create(0, &server_end1, &client_end1));
  auto control_client_unused_1 = fidl::Client<fuchsia_audio_device::Control>(
      fidl::ClientEnd<fuchsia_audio_device::Control>(std::move(client_end1)), dispatcher(),
      control_fidl_handler_.get());
  auto received_callback = false;
  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          .control_server = fidl::ServerEnd<fuchsia_audio_device::Control>(std::move(server_end1)),
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_ok());
      });
  RunLoopUntilIdle();
  ASSERT_TRUE(received_callback);

  zx::channel server_end2, client_end2;
  ASSERT_EQ(ZX_OK, zx::channel::create(0, &server_end2, &client_end2));
  auto control_client_unused_2 = fidl::Client<fuchsia_audio_device::Control>(
      fidl::ClientEnd<fuchsia_audio_device::Control>(std::move(client_end2)), dispatcher(),
      control_fidl_handler_.get());
  received_callback = false;
  control_creator->client()
      ->Create({{
          .token_id = *added_device_id,
          .control_server = fidl::ServerEnd<fuchsia_audio_device::Control>(std::move(server_end2)),
      }})
      .Then([&received_callback](fidl::Result<ControlCreator::Create>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ControlCreatorError::kDeviceAlreadyAllocated);
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// TODO(fxbug/dev:117199): When Health can change post-initialization, test: Healthy device becomes
// unhealthy before ControlCreator/Create. Expect Obs/Ctl/RingBuffer to drop & Reg/WatcDevRemoved.

}  // namespace
}  // namespace media_audio
