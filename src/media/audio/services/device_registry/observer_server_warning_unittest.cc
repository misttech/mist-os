// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <zircon/errors.h>

#include <memory>
#include <optional>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/observer_server.h"
#include "src/media/audio/services/device_registry/testing/fake_audio_driver.h"

namespace media_audio {

class ObserverServerWarningTest : public AudioDeviceRegistryServerTestBase,
                                  public fidl::AsyncEventHandler<fuchsia_audio_device::Observer> {
 protected:
  std::unique_ptr<FakeAudioDriver> CreateAndEnableDriverWithDefaults() {
    EXPECT_EQ(dispatcher(), test_loop().dispatcher());
    zx::channel server_end, client_end;
    EXPECT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
    auto fake_driver = std::make_unique<FakeAudioDriver>(std::move(server_end),
                                                         std::move(client_end), dispatcher());

    adr_service_->AddDevice(Device::Create(
        adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
        fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
    RunLoopUntilIdle();
    return fake_driver;
  }

  std::optional<TokenId> WaitForAddedDeviceTokenId(
      fidl::Client<fuchsia_audio_device::Registry>& registry_client) {
    std::optional<TokenId> added_device_id;
    registry_client->WatchDevicesAdded().Then(
        [&added_device_id](
            fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
          ASSERT_TRUE(result.is_ok());
          ASSERT_TRUE(result->devices());
          ASSERT_EQ(result->devices()->size(), 1u);
          ASSERT_TRUE(result->devices()->at(0).token_id());
          added_device_id = *result->devices()->at(0).token_id();
        });
    RunLoopUntilIdle();
    return added_device_id;
  }
};

// A subsequent call to WatchGainState before the previous one completes should fail.
TEST_F(ObserverServerWarningTest, WatchGainStateWhileAlreadyWatching) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);

  // We'll always receive an immediate response from the first `WatchGainState` call.
  auto observer = CreateTestObserverServer(added_device);
  bool received_initial_callback = false;
  observer->client()->WatchGainState().Then(
      [&received_initial_callback](
          fidl::Result<fuchsia_audio_device::Observer::WatchGainState>& result) mutable {
        received_initial_callback = true;
        ASSERT_TRUE(result.is_ok());
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_initial_callback);
  EXPECT_EQ(observer_fidl_error_status_.value_or(ZX_OK), ZX_OK);

  // The second `WatchGainState` call should pend indefinitely (even after the third one fails).
  observer->client()->WatchGainState().Then(
      [](fidl::Result<fuchsia_audio_device::Observer::WatchGainState>& result) mutable {
        FAIL() << "Unexpected completion for pending WatchGainState call";
      });
  RunLoopUntilIdle();

  // The third `WatchGainState` call should fail immediately (domain error WATCH_ALREADY_PENDING)
  // since the second call has not yet completed.
  bool received_expected_error_callback = false;
  observer->client()->WatchGainState().Then(
      [&received_expected_error_callback](
          fidl::Result<fuchsia_audio_device::Observer::WatchGainState>& result) mutable {
        received_expected_error_callback =
            result.is_error() && result.error_value().is_domain_error() &&
            (result.error_value().domain_error() ==
             fuchsia_audio_device::ObserverWatchGainStateError::kWatchAlreadyPending);

        ASSERT_TRUE(result.is_error()) << "Unexpected success to third WatchGainState";
        ASSERT_TRUE(result.error_value().is_domain_error())
            << "Unexpected framework error for third WatchGainState: "
            << result.error_value().FormatDescription();
        ASSERT_EQ(result.error_value().domain_error(),
                  result.error_value().domain_error().kWatchAlreadyPending)
            << "Unexpected domain error for third WatchGainState: "
            << result.error_value().FormatDescription();
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_expected_error_callback);
  EXPECT_EQ(observer_fidl_error_status_.value_or(ZX_OK), ZX_OK);
}

TEST_F(ObserverServerWarningTest, WatchPlugStateWhileAlreadyWatching) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);
  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);

  // We'll always receive an immediate response from the first `WatchPlugState` call.
  auto observer = CreateTestObserverServer(added_device);
  bool received_initial_callback = false;
  observer->client()->WatchPlugState().Then(
      [&received_initial_callback](
          fidl::Result<fuchsia_audio_device::Observer::WatchPlugState>& result) mutable {
        received_initial_callback = true;
        ASSERT_TRUE(result.is_ok());
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_initial_callback);
  EXPECT_EQ(observer_fidl_error_status_.value_or(ZX_OK), ZX_OK);

  // The second `WatchPlugState` call should pend indefinitely (even after the third one fails).
  observer->client()->WatchPlugState().Then(
      [](fidl::Result<fuchsia_audio_device::Observer::WatchPlugState>& result) mutable {
        FAIL() << "Unexpected completion for pending WatchPlugState call";
      });
  RunLoopUntilIdle();

  // The third `WatchPlugState` call should fail immediately (domain error WATCH_ALREADY_PENDING)
  // since the second call has not yet completed.
  bool received_expected_error_callback = false;
  observer->client()->WatchPlugState().Then(
      [&received_expected_error_callback](
          fidl::Result<fuchsia_audio_device::Observer::WatchPlugState>& result) mutable {
        received_expected_error_callback =
            result.is_error() && result.error_value().is_domain_error() &&
            (result.error_value().domain_error() ==
             fuchsia_audio_device::ObserverWatchPlugStateError::kWatchAlreadyPending);

        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error());
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ObserverWatchPlugStateError::kWatchAlreadyPending);
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_expected_error_callback);
  EXPECT_EQ(observer_fidl_error_status_.value_or(ZX_OK), ZX_OK);
}

// TODO(fxbug/dev:117199): When Health can change post-initialization, test: Healthy device becomes
//  unhealthy before WatchGainState. Expect Observer/Control/RingBuffer to drop, Reg/WatchRemove.

// TODO(fxbug/dev:117199): When Health can change post-initialization, test: Healthy device becomes
//  unhealthy before WatchPlugState. Expect Observer/Control/RingBuffer to drop, Reg/WatchRemove.

// TODO(fxbug/dev:117199): When Health can change post-initialization, test: Healthy device becomes
//  unhealthy before GetReferenceClock. Expect Observer/Control/RingBuffer to drop, Reg/WatchRemove.

}  // namespace media_audio
