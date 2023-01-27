// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <lib/sync/cpp/completion.h>
#include <zircon/errors.h>

#include <gtest/gtest.h>

#include "src/media/audio/services/common/testing/test_server_and_async_client.h"
#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/registry_server.h"

namespace media_audio {
namespace {

using Registry = fuchsia_audio_device::Registry;

class RegistryServerWarningTest : public AudioDeviceRegistryServerTestBase {};

// If the required 'id' field is not set, we should fail.
TEST_F(RegistryServerWarningTest, CreateObserverMissingToken) {
  auto registry = CreateTestRegistryServer();
  EXPECT_EQ(RegistryServer::count(), 1u);

  auto [observer_client_end, observer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Observer>();
  auto observer_client = fidl::Client<fuchsia_audio_device::Observer>(
      fidl::ClientEnd<fuchsia_audio_device::Observer>(std::move(observer_client_end)), dispatcher(),
      observer_fidl_handler_.get());
  bool received_callback = false;
  registry->client()
      ->CreateObserver({{
          .observer_server =
              fidl::ServerEnd<fuchsia_audio_device::Observer>(std::move(observer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Registry::CreateObserver>& result) {
        EXPECT_TRUE(result.is_error() && result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RegistryCreateObserverError::kInvalidTokenId);
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// If the 'id' field does not identify an initialized device, we should fail.
TEST_F(RegistryServerWarningTest, CreateObserverBadToken) {
  auto registry = CreateTestRegistryServer();
  EXPECT_EQ(RegistryServer::count(), 1u);

  auto [observer_client_end, observer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Observer>();
  auto observer_client = fidl::Client<fuchsia_audio_device::Observer>(
      fidl::ClientEnd<fuchsia_audio_device::Observer>(std::move(observer_client_end)), dispatcher(),
      observer_fidl_handler_.get());
  bool received_callback = false;
  registry->client()
      ->CreateObserver({{
          .token_id = 0,  // no device is present yet.
          .observer_server =
              fidl::ServerEnd<fuchsia_audio_device::Observer>(std::move(observer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Registry::CreateObserver>& result) {
        EXPECT_TRUE(result.is_error() && result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RegistryCreateObserverError::kDeviceNotFound);
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// If the required 'observer_server' field is not set, we should fail.
TEST_F(RegistryServerWarningTest, CreateObserverMissingObserver) {
  auto fake_driver = CreateFakeDriver();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);

  auto registry = CreateTestRegistryServer();
  EXPECT_EQ(RegistryServer::count(), 1u);

  std::optional<TokenId> id;
  registry->client()->WatchDevicesAdded().Then(
      [&id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
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
  auto observer_client = fidl::Client<fuchsia_audio_device::Observer>(
      fidl::ClientEnd<fuchsia_audio_device::Observer>(std::move(observer_client_end)), dispatcher(),
      observer_fidl_handler_.get());
  bool received_callback = false;
  registry->client()
      ->CreateObserver({{
          .token_id = *id,
      }})
      .Then([&received_callback](fidl::Result<Registry::CreateObserver>& result) {
        EXPECT_TRUE(result.is_error() && result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::RegistryCreateObserverError::kInvalidObserver);
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// If 'observer_server' is not set to a valid handle, we should fail.
TEST_F(RegistryServerWarningTest, CreateObserverBadObserver) {
  auto fake_driver = CreateFakeDriver();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  EXPECT_EQ(adr_service_->devices().size(), 1u);

  auto registry = CreateTestRegistryServer();
  EXPECT_EQ(RegistryServer::count(), 1u);

  std::optional<TokenId> id;
  registry->client()->WatchDevicesAdded().Then(
      [&id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
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
  auto observer_client = fidl::Client<fuchsia_audio_device::Observer>(
      fidl::ClientEnd<fuchsia_audio_device::Observer>(std::move(observer_client_end)), dispatcher(),
      observer_fidl_handler_.get());
  bool received_callback = false;
  registry->client()
      ->CreateObserver({{
          .token_id = *id,
          .observer_server = fidl::ServerEnd<fuchsia_audio_device::Observer>(zx::channel()),
      }})
      .Then([&received_callback](fidl::Result<Registry::CreateObserver>& result) {
        EXPECT_TRUE(result.is_error() && result.error_value().is_framework_error());
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_INVALID_ARGS);
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
}

// TODO(fxbug/dev:117199): When Health can change after initialization, we need a test case for:
// CreateObserver with token of device that was ready & Added, but then became unhealthy.

}  // namespace
}  // namespace media_audio
