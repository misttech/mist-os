// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <zircon/errors.h>

#include <memory>
#include <optional>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/observer_server.h"
#include "src/media/audio/services/device_registry/testing/fake_codec.h"
#include "src/media/audio/services/device_registry/testing/fake_composite.h"
#include "src/media/audio/services/device_registry/testing/fake_stream_config.h"

namespace media_audio {

using DriverClient = fuchsia_audio_device::DriverClient;

class ObserverServerWarningTest : public AudioDeviceRegistryServerTestBase,
                                  public fidl::AsyncEventHandler<fuchsia_audio_device::Observer> {
 protected:
  std::optional<TokenId> WaitForAddedDeviceTokenId(
      fidl::Client<fuchsia_audio_device::Registry>& registry_client) {
    std::optional<TokenId> added_device_id;
    registry_client->WatchDevicesAdded().Then(
        [&added_device_id](
            fidl::Result<fuchsia_audio_device::Registry::WatchDevicesAdded>& result) mutable {
          ASSERT_TRUE(result.is_ok()) << result.error_value();
          ASSERT_TRUE(result->devices());
          ASSERT_EQ(result->devices()->size(), 1u);
          ASSERT_TRUE(result->devices()->at(0).token_id());
          added_device_id = *result->devices()->at(0).token_id();
        });
    RunLoopUntilIdle();
    return added_device_id;
  }
};

class ObserverServerCodecWarningTest : public ObserverServerWarningTest {
 protected:
  std::shared_ptr<FakeCodec> CreateAndEnableDriverWithDefaults() {
    auto fake_driver = CreateFakeCodecNoDirection();

    adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test codec name",
                                           fuchsia_audio_device::DeviceType::kCodec,
                                           DriverClient::WithCodec(fake_driver->Enable())));
    RunLoopUntilIdle();
    return fake_driver;
  }
};

class ObserverServerCompositeWarningTest : public ObserverServerWarningTest {
 protected:
  std::shared_ptr<FakeComposite> CreateAndEnableDriverWithDefaults() {
    auto fake_driver = CreateFakeComposite();

    adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test composite name",
                                           fuchsia_audio_device::DeviceType::kComposite,
                                           DriverClient::WithComposite(fake_driver->Enable())));
    RunLoopUntilIdle();
    return fake_driver;
  }
};

class ObserverServerStreamConfigWarningTest : public ObserverServerWarningTest {
 protected:
  std::shared_ptr<FakeStreamConfig> CreateAndEnableDriverWithDefaults() {
    auto fake_driver = CreateFakeStreamConfigOutput();

    adr_service_->AddDevice(Device::Create(adr_service_, dispatcher(), "Test output name",
                                           fuchsia_audio_device::DeviceType::kOutput,
                                           DriverClient::WithStreamConfig(fake_driver->Enable())));
    RunLoopUntilIdle();
    return fake_driver;
  }
};

/////////////////////
// Codec tests
//
// Codec: WatchGainState is unsupported
TEST_F(ObserverServerCodecWarningTest, WatchGainStateWrongDeviceType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto observer = CreateTestObserverServer(added_device);
  ASSERT_EQ(ObserverServer::count(), 1u);
  bool received_callback = false;

  observer->client()->WatchGainState().Then(
      [&received_callback](
          fidl::Result<fuchsia_audio_device::Observer::WatchGainState>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().framework_error();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ObserverWatchGainStateError::kWrongDeviceType)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ObserverServer::count(), 1u);
  EXPECT_FALSE(observer_fidl_error_status_.has_value());
}

// While `WatchPlugState` is pending, calling this again is an error (but non-fatal).
TEST_F(ObserverServerCodecWarningTest, WatchPlugStateWhilePending) {
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
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_initial_callback);
  EXPECT_FALSE(observer_fidl_error_status_.has_value());
  bool received_second_callback = false;
  // The second `WatchPlugState` call should pend indefinitely (even after the third one fails).
  observer->client()->WatchPlugState().Then(
      [&received_second_callback](
          fidl::Result<fuchsia_audio_device::Observer::WatchPlugState>& result) mutable {
        received_second_callback = true;
        FAIL() << "Unexpected completion for pending WatchPlugState call";
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(received_second_callback);
  bool received_third_callback = false;
  // This third `WatchPlugState` call should fail immediately (domain error ALREADY_PENDING)
  // since the second call has not yet completed.
  observer->client()->WatchPlugState().Then(
      [&received_third_callback](
          fidl::Result<fuchsia_audio_device::Observer::WatchPlugState>& result) mutable {
        received_third_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ObserverWatchPlugStateError::kAlreadyPending)
            << result.error_value();
      });

  RunLoopUntilIdle();
  // After this, the second `WatchPlugState` should still pend and the Observer should still be OK.
  EXPECT_FALSE(received_second_callback);
  EXPECT_TRUE(received_third_callback);
  EXPECT_EQ(ObserverServer::count(), 1u);
  EXPECT_FALSE(observer_fidl_error_status_.has_value());
}

// Codec: GetReferenceClock is unsupported
TEST_F(ObserverServerCodecWarningTest, GetReferenceClockWrongDeviceType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto observer = CreateTestObserverServer(added_device);
  ASSERT_EQ(ObserverServer::count(), 1u);
  bool received_callback = false;

  observer->client()->GetReferenceClock().Then(
      [&received_callback](
          fidl::Result<fuchsia_audio_device::Observer::GetReferenceClock>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().framework_error();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ObserverGetReferenceClockError::kWrongDeviceType)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ObserverServer::count(), 1u);
  EXPECT_FALSE(observer_fidl_error_status_.has_value());
}

// TODO(https://fxbug.dev/323270827): implement signalprocessing for Codec (topology, gain).
// Add Codec negative test cases for WatchTopology and WatchElementState (once implemented)

/////////////////////
// Composite tests
//
// Verify that the Observer cannot handle a WatchGainState request from this type of device.
TEST_F(ObserverServerCompositeWarningTest, WatchGainStateWrongDeviceType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto observer = CreateTestObserverServer(added_device);
  ASSERT_EQ(ObserverServer::count(), 1u);
  bool received_callback = false;

  observer->client()->WatchGainState().Then(
      [&received_callback](
          fidl::Result<fuchsia_audio_device::Observer::WatchGainState>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().framework_error();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ObserverWatchGainStateError::kWrongDeviceType)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ObserverServer::count(), 1u);
  EXPECT_FALSE(observer_fidl_error_status_.has_value());
}

// Verify that the Observer cannot handle a WatchPlugState request from this type of device.
TEST_F(ObserverServerCompositeWarningTest, WatchPlugStateWrongDeviceType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto observer = CreateTestObserverServer(added_device);
  ASSERT_EQ(ObserverServer::count(), 1u);
  bool received_callback = false;

  observer->client()->WatchPlugState().Then(
      [&received_callback](
          fidl::Result<fuchsia_audio_device::Observer::WatchPlugState>& result) mutable {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error())
            << result.error_value().framework_error();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ObserverWatchPlugStateError::kWrongDeviceType)
            << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_EQ(ObserverServer::count(), 1u);
  EXPECT_FALSE(observer_fidl_error_status_.has_value());
}

/////////////////////
// StreamConfig tests
//
// A subsequent call to WatchGainState before the previous one completes should fail.
TEST_F(ObserverServerStreamConfigWarningTest, WatchGainStateWhilePending) {
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
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_initial_callback);
  EXPECT_FALSE(observer_fidl_error_status_.has_value());
  // EXPECT_EQ(observer_fidl_error_status_.value_or(ZX_OK), ZX_OK);
  bool received_second_callback = false;
  // The second `WatchGainState` call should pend indefinitely (even after the third one fails).
  observer->client()->WatchGainState().Then(
      [&received_second_callback](
          fidl::Result<fuchsia_audio_device::Observer::WatchGainState>& result) mutable {
        received_second_callback = true;
        FAIL() << "Unexpected completion for pending WatchGainState call";
      });

  RunLoopUntilIdle();
  // The third `WatchGainState` call should fail immediately (domain error ALREADY_PENDING)
  // since the second call has not yet completed.
  EXPECT_FALSE(received_second_callback);
  bool received_third_callback = false;
  observer->client()->WatchGainState().Then(
      [&received_third_callback](
          fidl::Result<fuchsia_audio_device::Observer::WatchGainState>& result) mutable {
        received_third_callback = true;
        ASSERT_TRUE(result.is_error()) << "Unexpected success to third WatchGainState";
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ObserverWatchGainStateError::kAlreadyPending)
            << result.error_value();
      });

  RunLoopUntilIdle();
  // After this, the second `WatchGainState` should still pend and the Observer should still be OK.
  EXPECT_FALSE(received_second_callback);
  EXPECT_TRUE(received_third_callback);
  EXPECT_EQ(ObserverServer::count(), 1u);
  EXPECT_FALSE(observer_fidl_error_status_.has_value());
}

TEST_F(ObserverServerStreamConfigWarningTest, WatchPlugStateWhilePending) {
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
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_initial_callback);
  EXPECT_FALSE(observer_fidl_error_status_.has_value());
  // EXPECT_EQ(observer_fidl_error_status_.value_or(ZX_OK), ZX_OK);
  bool received_second_callback = false;
  // The second `WatchPlugState` call should pend indefinitely (even after the third one fails).
  observer->client()->WatchPlugState().Then(
      [&received_second_callback](
          fidl::Result<fuchsia_audio_device::Observer::WatchPlugState>& result) mutable {
        received_second_callback = true;
        FAIL() << "Unexpected completion for pending WatchPlugState call";
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(received_second_callback);
  bool received_third_callback = false;
  // This third `WatchPlugState` call should fail immediately (domain error ALREADY_PENDING)
  // since the second call has not yet completed.
  observer->client()->WatchPlugState().Then(
      [&received_third_callback](
          fidl::Result<fuchsia_audio_device::Observer::WatchPlugState>& result) mutable {
        received_third_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_domain_error()) << result.error_value();
        EXPECT_EQ(result.error_value().domain_error(),
                  fuchsia_audio_device::ObserverWatchPlugStateError::kAlreadyPending)
            << result.error_value();
      });

  RunLoopUntilIdle();
  // After this, the second `WatchPlugState` should still pend and the Observer should still be OK.
  EXPECT_FALSE(received_second_callback);
  EXPECT_TRUE(received_third_callback);
  EXPECT_EQ(ObserverServer::count(), 1u);
  EXPECT_FALSE(observer_fidl_error_status_.has_value());
}

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test: device becomes
//   unhealthy before WatchGainState. Expect Obs/Ctl/RingBuf to drop + Reg/WatchRemove notif.

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test: device becomes
//   unhealthy before WatchPlugState. Expect Obs/Ctl/RingBuf to drop + Reg/WatchRemove notif.

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test: device becomes
//   unhealthy before GetReferenceClock. Expect Obs/Ctl/RingBuf to drop + Reg/WatchRemove notif.

}  // namespace media_audio
