// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.audio.device/cpp/common_types.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
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
using Observer = fuchsia_audio_device::Observer;

class ObserverServerWarningTest : public AudioDeviceRegistryServerTestBase,
                                  public fidl::AsyncEventHandler<Observer> {
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

    adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test codec name",
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

    adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test composite name",
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

    adr_service()->AddDevice(Device::Create(adr_service(), dispatcher(), "Test output name",
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
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto observer = CreateTestObserverServer(added_device);
  ASSERT_EQ(ObserverServer::count(), 1u);
  bool received_callback = false;

  observer->client()->WatchGainState().Then(
      [&received_callback](fidl::Result<Observer::WatchGainState>& result) mutable {
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
  EXPECT_FALSE(observer_fidl_error_status().has_value()) << *observer_fidl_error_status();
}

// While `WatchPlugState` is pending, calling this again is an error (but non-fatal).
TEST_F(ObserverServerCodecWarningTest, WatchPlugStateWhilePending) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  // We'll always receive an immediate response from the first `WatchPlugState` call.
  auto observer = CreateTestObserverServer(added_device);
  bool received_initial_callback = false;
  observer->client()->WatchPlugState().Then(
      [&received_initial_callback](fidl::Result<Observer::WatchPlugState>& result) mutable {
        received_initial_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_initial_callback);
  EXPECT_FALSE(observer_fidl_error_status().has_value()) << *observer_fidl_error_status();
  bool received_second_callback = false;
  // The second `WatchPlugState` call should pend indefinitely (even after the third one fails).
  observer->client()->WatchPlugState().Then(
      [&received_second_callback](fidl::Result<Observer::WatchPlugState>& result) mutable {
        received_second_callback = true;
        FAIL() << "Unexpected completion for pending WatchPlugState call";
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(received_second_callback);
  bool received_third_callback = false;
  // This third `WatchPlugState` call should fail immediately (domain error ALREADY_PENDING)
  // since the second call has not yet completed.
  observer->client()->WatchPlugState().Then(
      [&received_third_callback](fidl::Result<Observer::WatchPlugState>& result) mutable {
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
  EXPECT_FALSE(observer_fidl_error_status().has_value()) << *observer_fidl_error_status();
}

// Codec: GetReferenceClock is unsupported
TEST_F(ObserverServerCodecWarningTest, GetReferenceClockWrongDeviceType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto observer = CreateTestObserverServer(added_device);
  ASSERT_EQ(ObserverServer::count(), 1u);
  bool received_callback = false;

  observer->client()->GetReferenceClock().Then(
      [&received_callback](fidl::Result<Observer::GetReferenceClock>& result) mutable {
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
  EXPECT_FALSE(observer_fidl_error_status().has_value()) << *observer_fidl_error_status();
}

// TODO(https://fxbug.dev/323270827): implement signalprocessing for Codec (topology, gain),
// including in the FakeCodec test fixture. Then add negative test cases for
// GetTopologies/GetElements/WatchTopology/WatchElementState, as are in Composite.

// Verify WatchTopology if the driver does not support signalprocessing.
TEST_F(ObserverServerCodecWarningTest, WatchTopologyUnsupported) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  ASSERT_FALSE(device->info()->signal_processing_topologies().has_value());
  auto observer = CreateTestObserverServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ObserverServer::count(), 1u);
  auto received_callback = false;

  observer->client()->WatchTopology().Then(
      [&received_callback](fidl::Result<Observer::WatchTopology>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_NOT_SUPPORTED);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  received_callback = false;

  // After this failing call, the binding should not be usable.
  observer->client()->WatchPlugState().Then(
      [&received_callback](fidl::Result<Observer::WatchPlugState>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error());
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_NOT_SUPPORTED);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_TRUE(observer->client().is_valid());
}

// Verify WatchElementState if the driver does not support signalprocessing.
TEST_F(ObserverServerCodecWarningTest, WatchElementStateUnsupported) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  ASSERT_FALSE(device->info()->signal_processing_topologies().has_value());
  auto observer = CreateTestObserverServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ObserverServer::count(), 1u);
  auto received_callback = false;

  observer->client()
      ->WatchElementState(fuchsia_audio_device::kDefaultDaiInterconnectElementId)
      .Then([&received_callback](fidl::Result<Observer::WatchElementState>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_NOT_SUPPORTED);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  received_callback = false;

  // After this failing call, the binding should not be usable.
  observer->client()->WatchPlugState().Then(
      [&received_callback](fidl::Result<Observer::WatchPlugState>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error());
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_NOT_SUPPORTED);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_TRUE(observer->client().is_valid());
}

/////////////////////
// Composite tests
//
// Verify that the Observer cannot handle a WatchGainState request from this type of device.
TEST_F(ObserverServerCompositeWarningTest, WatchGainStateWrongDeviceType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto observer = CreateTestObserverServer(added_device);
  ASSERT_EQ(ObserverServer::count(), 1u);
  bool received_callback = false;

  observer->client()->WatchGainState().Then(
      [&received_callback](fidl::Result<Observer::WatchGainState>& result) mutable {
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
  EXPECT_FALSE(observer_fidl_error_status().has_value()) << *observer_fidl_error_status();
}

// Verify that the Observer cannot handle a WatchPlugState request from this type of device.
TEST_F(ObserverServerCompositeWarningTest, WatchPlugStateWrongDeviceType) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto observer = CreateTestObserverServer(added_device);
  ASSERT_EQ(ObserverServer::count(), 1u);
  bool received_callback = false;

  observer->client()->WatchPlugState().Then(
      [&received_callback](fidl::Result<Observer::WatchPlugState>& result) mutable {
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
  EXPECT_FALSE(observer_fidl_error_status().has_value()) << *observer_fidl_error_status();
}

// WatchTopology cases (without using SetTopology): Watch-while-pending
TEST_F(ObserverServerCompositeWarningTest, WatchTopologyWhilePending) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto observer = CreateTestObserverServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ObserverServer::count(), 1u);
  auto received_callback1 = false, received_callback2 = false;

  observer->client()->WatchTopology().Then(
      [&received_callback1](fidl::Result<Observer::WatchTopology>& result) {
        received_callback1 = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback1);
  received_callback1 = false;

  observer->client()->WatchTopology().Then(
      [&received_callback1](fidl::Result<Observer::WatchTopology>& result) {
        received_callback1 = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_BAD_STATE);
        // EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(received_callback1);

  observer->client()->WatchTopology().Then(
      [&received_callback2](fidl::Result<Observer::WatchTopology>& result) {
        received_callback2 = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_BAD_STATE);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback2);
  // After a failing WatchTopology call, the binding should not be usable, so the previous
  // WatchElementState will complete with a failure.
  EXPECT_TRUE(received_callback1);
}

// WatchElementState cases (without SetElementState): unknown ElementId, Watch-while-pending
TEST_F(ObserverServerCompositeWarningTest, WatchElementStateUnknownElementId) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto observer = CreateTestObserverServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ObserverServer::count(), 1u);
  auto& elements_from_device = element_map(device);
  ElementId unknown_element_id = 0;
  while (true) {
    if (elements_from_device.find(unknown_element_id) == elements_from_device.end()) {
      break;
    }
    ++unknown_element_id;
  }
  auto received_callback = false;

  observer->client()
      ->WatchElementState(unknown_element_id)
      .Then([&received_callback](fidl::Result<Observer::WatchElementState>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_INVALID_ARGS);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  // After a failing WatchElementState call, the binding should not be usable.
  observer->client()->GetReferenceClock().Then(
      [&received_callback](fidl::Result<Observer::GetReferenceClock>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error());
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_INVALID_ARGS);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_TRUE(observer->client().is_valid());
}

TEST_F(ObserverServerCompositeWarningTest, WatchElementStateWhilePending) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto observer = CreateTestObserverServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ObserverServer::count(), 1u);
  auto& elements_from_device = element_map(device);

  auto element_id = elements_from_device.begin()->first;
  auto received_callback1 = false, received_callback2 = false;

  observer->client()
      ->WatchElementState(element_id)
      .Then([&received_callback1](fidl::Result<Observer::WatchElementState>& result) {
        received_callback1 = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback1);
  received_callback1 = false;

  observer->client()
      ->WatchElementState(element_id)
      .Then([&received_callback1](fidl::Result<Observer::WatchElementState>& result) {
        received_callback1 = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_BAD_STATE) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(received_callback1);

  observer->client()
      ->WatchElementState(element_id)
      .Then([&received_callback2](fidl::Result<Observer::WatchElementState>& result) {
        received_callback2 = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_BAD_STATE) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback2);
  // After a failing WatchElementState call, the binding should not be usable, so the previous
  // WatchElementState will complete with a failure.
  EXPECT_TRUE(received_callback1);
  received_callback1 = false;

  observer->client()->GetReferenceClock().Then(
      [&received_callback1](fidl::Result<Observer::GetReferenceClock>& result) {
        received_callback1 = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error()) << result.error_value();
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_BAD_STATE)
            << result.error_value().framework_error();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback1);
  EXPECT_TRUE(observer->client().is_valid());
}

/////////////////////
// StreamConfig tests
//
// A subsequent call to WatchGainState before the previous one completes should fail.
TEST_F(ObserverServerStreamConfigWarningTest, WatchGainStateWhilePending) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  // We'll always receive an immediate response from the first `WatchGainState` call.
  auto observer = CreateTestObserverServer(added_device);
  bool received_initial_callback = false;
  observer->client()->WatchGainState().Then(
      [&received_initial_callback](fidl::Result<Observer::WatchGainState>& result) mutable {
        received_initial_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_initial_callback);
  EXPECT_FALSE(observer_fidl_error_status().has_value()) << *observer_fidl_error_status();
  // EXPECT_EQ(observer_fidl_error_status().value_or(ZX_OK), ZX_OK);
  bool received_second_callback = false;
  // The second `WatchGainState` call should pend indefinitely (even after the third one fails).
  observer->client()->WatchGainState().Then(
      [&received_second_callback](fidl::Result<Observer::WatchGainState>& result) mutable {
        received_second_callback = true;
        FAIL() << "Unexpected completion for pending WatchGainState call";
      });

  RunLoopUntilIdle();
  // The third `WatchGainState` call should fail immediately (domain error ALREADY_PENDING)
  // since the second call has not yet completed.
  EXPECT_FALSE(received_second_callback);
  bool received_third_callback = false;
  observer->client()->WatchGainState().Then(
      [&received_third_callback](fidl::Result<Observer::WatchGainState>& result) mutable {
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
  EXPECT_FALSE(observer_fidl_error_status().has_value()) << *observer_fidl_error_status();
}

TEST_F(ObserverServerStreamConfigWarningTest, WatchPlugStateWhilePending) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service()->devices().size(), 1u);
  ASSERT_EQ(adr_service()->unhealthy_devices().size(), 0u);
  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  // We'll always receive an immediate response from the first `WatchPlugState` call.
  auto observer = CreateTestObserverServer(added_device);
  bool received_initial_callback = false;
  observer->client()->WatchPlugState().Then(
      [&received_initial_callback](fidl::Result<Observer::WatchPlugState>& result) mutable {
        received_initial_callback = true;
        EXPECT_TRUE(result.is_ok()) << result.error_value();
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_initial_callback);
  EXPECT_FALSE(observer_fidl_error_status().has_value()) << *observer_fidl_error_status();
  // EXPECT_EQ(observer_fidl_error_status().value_or(ZX_OK), ZX_OK);
  bool received_second_callback = false;
  // The second `WatchPlugState` call should pend indefinitely (even after the third one fails).
  observer->client()->WatchPlugState().Then(
      [&received_second_callback](fidl::Result<Observer::WatchPlugState>& result) mutable {
        received_second_callback = true;
        FAIL() << "Unexpected completion for pending WatchPlugState call";
      });

  RunLoopUntilIdle();
  EXPECT_FALSE(received_second_callback);
  bool received_third_callback = false;
  // This third `WatchPlugState` call should fail immediately (domain error ALREADY_PENDING)
  // since the second call has not yet completed.
  observer->client()->WatchPlugState().Then(
      [&received_third_callback](fidl::Result<Observer::WatchPlugState>& result) mutable {
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
  EXPECT_FALSE(observer_fidl_error_status().has_value()) << *observer_fidl_error_status();
}

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test: device becomes
//   unhealthy before WatchGainState. Expect Obs/Ctl/RingBuf to drop + Reg/WatchRemove notif.

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test: device becomes
//   unhealthy before WatchPlugState. Expect Obs/Ctl/RingBuf to drop + Reg/WatchRemove notif.

// TODO(https://fxbug.dev/42068381): If Health can change post-initialization, test: device becomes
//   unhealthy before GetReferenceClock. Expect Obs/Ctl/RingBuf to drop + Reg/WatchRemove notif.

// TODO(https://fxbug.dev/323270827): implement signalprocessing, including in the FakeStreamConfig
// test fixture. Then add negative test cases for
// GetTopologies/GetElements/WatchTopology/WatchElementState, as are in Composite.

// Verify WatchTopology if the driver does not support signalprocessing.
TEST_F(ObserverServerStreamConfigWarningTest, WatchTopologyUnsupported) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  ASSERT_FALSE(device->info()->signal_processing_topologies().has_value());
  auto observer = CreateTestObserverServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ObserverServer::count(), 1u);
  auto received_callback = false;

  observer->client()->WatchTopology().Then(
      [&received_callback](fidl::Result<Observer::WatchTopology>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_NOT_SUPPORTED);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  received_callback = false;

  // After a failing WatchTopology call, the binding should not be usable.
  observer->client()->GetReferenceClock().Then(
      [&received_callback](fidl::Result<Observer::GetReferenceClock>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error());
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_NOT_SUPPORTED);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_TRUE(observer->client().is_valid());
}

// Verify WatchElementState if the driver does not support signalprocessing.
TEST_F(ObserverServerStreamConfigWarningTest, WatchElementStateUnsupported) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto registry = CreateTestRegistryServer();

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, device] = adr_service()->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  ASSERT_FALSE(device->info()->signal_processing_topologies().has_value());
  auto observer = CreateTestObserverServer(device);

  RunLoopUntilIdle();
  ASSERT_EQ(RegistryServer::count(), 1u);
  ASSERT_EQ(ObserverServer::count(), 1u);
  auto received_callback = false;

  observer->client()
      ->WatchElementState(fuchsia_audio_device::kDefaultDaiInterconnectElementId)
      .Then([&received_callback](fidl::Result<Observer::WatchElementState>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        EXPECT_EQ(result.error_value().status(), ZX_ERR_NOT_SUPPORTED);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  received_callback = false;

  // After a failing WatchElementState call, the binding should not be usable.
  observer->client()->GetReferenceClock().Then(
      [&received_callback](fidl::Result<Observer::GetReferenceClock>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_error());
        ASSERT_TRUE(result.error_value().is_framework_error());
        EXPECT_EQ(result.error_value().framework_error().status(), ZX_ERR_NOT_SUPPORTED);
      });

  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_TRUE(observer->client().is_valid());
}

}  // namespace media_audio
