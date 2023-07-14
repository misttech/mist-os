// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/observer_server.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <zircon/errors.h>

#include <memory>
#include <optional>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/testing/fake_audio_driver.h"

namespace media_audio {

using Control = fuchsia_audio_device::Control;
using Observer = fuchsia_audio_device::Observer;
using Registry = fuchsia_audio_device::Registry;

class ObserverServerTest : public AudioDeviceRegistryServerTestBase,
                           public fidl::AsyncEventHandler<fuchsia_audio_device::Observer> {
 protected:
  static inline const fuchsia_audio::Format kDefaultRingBufferFormat{{
      .sample_type = fuchsia_audio::SampleType::kInt16,
      .channel_count = 2,
      .frames_per_second = 48000,
  }};

  std::unique_ptr<FakeAudioDriver> CreateAndEnableDriverWithDefaults();

  std::optional<TokenId> WaitForAddedDeviceTokenId(
      fidl::Client<fuchsia_audio_device::Registry>& registry_client);
  std::optional<TokenId> WaitForRemovedDeviceTokenId(
      fidl::Client<fuchsia_audio_device::Registry>& registry_client);

  std::pair<fidl::Client<fuchsia_audio_device::RingBuffer>,
            fidl::ServerEnd<fuchsia_audio_device::RingBuffer>>
  CreateRingBufferClient();

  fidl::Client<fuchsia_audio_device::Observer> ConnectToObserver(
      fidl::Client<fuchsia_audio_device::Registry>& registry_client, TokenId token_id);
};

std::unique_ptr<FakeAudioDriver> ObserverServerTest::CreateAndEnableDriverWithDefaults() {
  EXPECT_EQ(dispatcher(), test_loop().dispatcher());
  zx::channel server_end, client_end;
  EXPECT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
  auto fake_driver =
      std::make_unique<FakeAudioDriver>(std::move(server_end), std::move(client_end), dispatcher());

  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  return fake_driver;
}

std::optional<TokenId> ObserverServerTest::WaitForAddedDeviceTokenId(
    fidl::Client<fuchsia_audio_device::Registry>& registry_client) {
  std::optional<TokenId> added_device_id;
  registry_client->WatchDevicesAdded().Then(
      [&added_device_id](fidl::Result<Registry::WatchDevicesAdded>& result) mutable {
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->devices());
        ASSERT_EQ(result->devices()->size(), 1u);
        ASSERT_TRUE(result->devices()->at(0).token_id());
        added_device_id = *result->devices()->at(0).token_id();
      });
  RunLoopUntilIdle();
  return added_device_id;
}

std::optional<TokenId> ObserverServerTest::WaitForRemovedDeviceTokenId(
    fidl::Client<fuchsia_audio_device::Registry>& registry_client) {
  std::optional<TokenId> removed_device_id;
  registry_client->WatchDeviceRemoved().Then(
      [&removed_device_id](fidl::Result<Registry::WatchDeviceRemoved>& result) mutable {
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->token_id());
        removed_device_id = *result->token_id();
      });
  RunLoopUntilIdle();
  return removed_device_id;
}

std::pair<fidl::Client<fuchsia_audio_device::RingBuffer>,
          fidl::ServerEnd<fuchsia_audio_device::RingBuffer>>
ObserverServerTest::CreateRingBufferClient() {
  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::RingBuffer>();
  auto ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>(
      fidl::ClientEnd<fuchsia_audio_device::RingBuffer>(std::move(ring_buffer_client_end)),
      dispatcher());
  return std::make_pair(std::move(ring_buffer_client), std::move(ring_buffer_server_end));
}

fidl::Client<fuchsia_audio_device::Observer> ObserverServerTest::ConnectToObserver(
    fidl::Client<fuchsia_audio_device::Registry>& registry_client, TokenId token_id) {
  auto [observer_client_end, observer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Observer>();
  auto observer_client = fidl::Client<fuchsia_audio_device::Observer>(
      fidl::ClientEnd<fuchsia_audio_device::Observer>(std::move(observer_client_end)), dispatcher(),
      this);
  bool received_callback = false;
  registry_client
      ->CreateObserver({{
          .token_id = token_id,
          .observer_server =
              fidl::ServerEnd<fuchsia_audio_device::Observer>(std::move(observer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Registry::CreateObserver>& result) {
        ASSERT_TRUE(result.is_ok());
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_TRUE(observer_client.is_valid());
  return observer_client;
}

// Validate that an Observer client can drop cleanly (without generating a WARNING or ERROR).
TEST_F(ObserverServerTest, CleanClientDrop) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto observer = CreateTestObserverServer(*adr_service_->devices().begin());
  ASSERT_EQ(ObserverServer::count(), 1u);

  observer->client() = fidl::Client<fuchsia_audio_device::Observer>();
}

// Validate that an Observer server can shutdown cleanly (without generating a WARNING or ERROR).
TEST_F(ObserverServerTest, CleanServerShutdown) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto observer = CreateTestObserverServer(*adr_service_->devices().begin());
  ASSERT_EQ(ObserverServer::count(), 1u);

  observer->server().Shutdown(ZX_ERR_PEER_CLOSED);
}

// Validate creation of an Observer via the Registry/CreateObserver method. Most other test cases
// directly creeate an Observer server and client synthetically via CreateTestObserverServer.
TEST_F(ObserverServerTest, Creation) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);

  auto [observer_client_end, observer_server_end] =
      CreateNaturalAsyncClientOrDie<fuchsia_audio_device::Observer>();
  auto observer_client = fidl::Client<fuchsia_audio_device::Observer>(
      fidl::ClientEnd<fuchsia_audio_device::Observer>(std::move(observer_client_end)), dispatcher(),
      observer_fidl_handler_.get());
  bool received_callback = false;
  registry->client()
      ->CreateObserver({{
          .token_id = *added_device_id,
          .observer_server =
              fidl::ServerEnd<fuchsia_audio_device::Observer>(std::move(observer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<Registry::CreateObserver>& result) {
        ASSERT_TRUE(result.is_ok());
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_TRUE(observer_client.is_valid());
  EXPECT_FALSE(observer_fidl_error_status_);

  observer_client = fidl::Client<fuchsia_audio_device::Observer>();
}

// Validate that when an observed device is removed, the Observer is dropped.
TEST_F(ObserverServerTest, ObservedDeviceRemoved) {
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

  fake_driver->DropStreamConfig();
  RunLoopUntilIdle();
  auto removed_device_id = WaitForRemovedDeviceTokenId(registry->client());
  ASSERT_TRUE(removed_device_id);

  RunLoopUntilIdle();
  EXPECT_TRUE(observer_fidl_error_status_);
  EXPECT_EQ(*observer_fidl_error_status_, ZX_ERR_PEER_CLOSED);
}

// Validate that the Observer receives the initial gain state of the observed device.
TEST_F(ObserverServerTest, InitialGainState) {
  zx::channel server_end, client_end;
  EXPECT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
  auto fake_driver =
      std::make_unique<FakeAudioDriver>(std::move(server_end), std::move(client_end), dispatcher());
  constexpr float kGainDb = -2.0f;
  fake_driver->InjectGainChange({{
      .muted = true,
      .agc_enabled = true,
      .gain_db = kGainDb,
  }});
  RunLoopUntilIdle();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto observer = CreateTestObserverServer(added_device);

  bool received_callback = false;
  observer->client()->WatchGainState().Then(
      [&received_callback, kGainDb](fidl::Result<Observer::WatchGainState>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->state());
        ASSERT_TRUE(result->state()->gain_db());
        EXPECT_EQ(*result->state()->gain_db(), kGainDb);
        EXPECT_TRUE(*result->state()->muted());
        EXPECT_TRUE(*result->state()->agc_enabled());
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  EXPECT_FALSE(observer_fidl_error_status_);
}

// Validate that the Observer receives changes in the gain state of the observed device.
TEST_F(ObserverServerTest, GainChange) {
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

  bool received_callback = false;
  observer->client()->WatchGainState().Then(
      [&received_callback](fidl::Result<Observer::WatchGainState>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->state());
        ASSERT_TRUE(result->state()->gain_db());
        EXPECT_EQ(*result->state()->gain_db(), 0.0f);
        EXPECT_FALSE(*result->state()->muted());
        EXPECT_FALSE(*result->state()->agc_enabled());
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  constexpr float kGainDb = -2.0f;
  received_callback = false;
  observer->client()->WatchGainState().Then(
      [&received_callback, kGainDb](fidl::Result<Observer::WatchGainState>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->state());
        ASSERT_TRUE(result->state()->gain_db());
        EXPECT_EQ(*result->state()->gain_db(), kGainDb);
        EXPECT_TRUE(*result->state()->muted());
        EXPECT_TRUE(*result->state()->agc_enabled());
      });
  RunLoopUntilIdle();
  EXPECT_FALSE(received_callback);

  fake_driver->InjectGainChange({{
      .muted = true,
      .agc_enabled = true,
      .gain_db = kGainDb,
  }});
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(observer_fidl_error_status_);
}

// Validate that the Observer receives the initial plug state of the observed device.
TEST_F(ObserverServerTest, InitialPlugState) {
  zx::channel server_end, client_end;
  EXPECT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
  auto fake_driver =
      std::make_unique<FakeAudioDriver>(std::move(server_end), std::move(client_end), dispatcher());
  auto initial_plug_time = zx::clock::get_monotonic();
  fake_driver->InjectPlugChange(false, initial_plug_time);
  RunLoopUntilIdle();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fuchsia_audio_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto observer = CreateTestObserverServer(added_device);

  bool received_callback = false;
  observer->client()->WatchPlugState().Then(
      [&received_callback, initial_plug_time](fidl::Result<Observer::WatchPlugState>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->state());
        EXPECT_EQ(*result->state(), fuchsia_audio_device::PlugState::kUnplugged);
        ASSERT_TRUE(result->plug_time());
        EXPECT_EQ(*result->plug_time(), initial_plug_time.get());
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(observer_fidl_error_status_);
}

// Validate that the Observer receives changes in the plug state of the observed device.
TEST_F(ObserverServerTest, PlugChange) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  auto registry = CreateTestRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());
  ASSERT_TRUE(added_device_id);
  auto time_of_plug_change = zx::clock::get_monotonic();

  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto observer = CreateTestObserverServer(added_device);

  bool received_callback = false;
  observer->client()->WatchPlugState().Then(
      [&received_callback, time_of_plug_change](fidl::Result<Observer::WatchPlugState>& result) {
        FX_LOGS(DEBUG) << "Received callback 1";
        received_callback = true;
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->state());
        EXPECT_EQ(*result->state(), fuchsia_audio_device::PlugState::kPlugged);
        ASSERT_TRUE(result->plug_time());
        EXPECT_LT(*result->plug_time(), time_of_plug_change.get());
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  received_callback = false;
  observer->client()->WatchPlugState().Then(
      [&received_callback, time_of_plug_change](fidl::Result<Observer::WatchPlugState>& result) {
        FX_LOGS(DEBUG) << "Received callback 2";
        received_callback = true;
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->state());
        EXPECT_EQ(*result->state(), fuchsia_audio_device::PlugState::kUnplugged);
        ASSERT_TRUE(result->plug_time());
        EXPECT_EQ(*result->plug_time(), time_of_plug_change.get());
      });
  RunLoopUntilIdle();
  EXPECT_FALSE(received_callback);

  fake_driver->InjectPlugChange(false, time_of_plug_change);
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(observer_fidl_error_status_);
}

// Validate that the Observer receives the observed device's reference clock, and that it is valid.
TEST_F(ObserverServerTest, GetReferenceClock) {
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

  bool received_callback = false;
  observer->client()->GetReferenceClock().Then(
      [&received_callback](fidl::Result<Observer::GetReferenceClock>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->reference_clock());
        zx::clock clock = std::move(*result->reference_clock());
        EXPECT_TRUE(clock.is_valid());
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_FALSE(observer_fidl_error_status_);
}

// Validate that an Observer does not drop, if the observed device's driver RingBuffer is dropped.
TEST_F(ObserverServerTest, ObserverDoesNotDropIfDriverRingBufferDrops) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);

  auto registry = CreateTestRegistryServer();
  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());

  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(added_device);
  auto observer = CreateTestObserverServer(added_device);

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control->client()
      ->CreateRingBuffer(
          {{.options = fuchsia_audio_device::RingBufferOptions{{.format = kDefaultRingBufferFormat,
                                                                .ring_buffer_min_bytes = 2000}},
            .ring_buffer_server = fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(
                std::move(ring_buffer_server_end))}})
      .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  fake_driver->DropRingBuffer();
  RunLoopUntilIdle();

  EXPECT_EQ(ObserverServer::count(), 1u);
  EXPECT_TRUE(observer->client().is_valid());
  EXPECT_FALSE(observer_fidl_error_status_);

  ring_buffer_client = fidl::Client<fuchsia_audio_device::RingBuffer>();
}

// Validate that an Observer does not drop, if the observed device's RingBuffer client is dropped.
TEST_F(ObserverServerTest, ObserverDoesNotDropIfClientRingBufferDrops) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);

  auto registry = CreateTestRegistryServer();
  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());

  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto control = CreateTestControlServer(added_device);
  auto observer = CreateTestObserverServer(added_device);

  {
    auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
    bool received_callback = false;
    control->client()
        ->CreateRingBuffer(
            {{.options =
                  fuchsia_audio_device::RingBufferOptions{
                      {.format = kDefaultRingBufferFormat, .ring_buffer_min_bytes = 2000}},
              .ring_buffer_server = fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(
                  std::move(ring_buffer_server_end))}})
        .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
          received_callback = true;
          ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        });
    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);
  }
  RunLoopUntilIdle();

  EXPECT_EQ(ObserverServer::count(), 1u);
  EXPECT_TRUE(observer->client().is_valid());
  EXPECT_FALSE(observer_fidl_error_status_);
}

// Validate that an Observer does not drop, if the observed device's Control client is dropped.
TEST_F(ObserverServerTest, ObserverDoesNotDropIfClientControlDrops) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);

  auto registry = CreateTestRegistryServer();
  auto added_device_id = WaitForAddedDeviceTokenId(registry->client());

  ASSERT_TRUE(added_device_id);
  auto [status, added_device] = adr_service_->FindDeviceByTokenId(*added_device_id);
  ASSERT_EQ(status, AudioDeviceRegistry::DevicePresence::Active);
  auto observer = CreateTestObserverServer(added_device);

  {
    auto control = CreateTestControlServer(added_device);
    auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
    bool received_callback = false;
    control->client()
        ->CreateRingBuffer(
            {{.options =
                  fuchsia_audio_device::RingBufferOptions{
                      {.format = kDefaultRingBufferFormat, .ring_buffer_min_bytes = 2000}},
              .ring_buffer_server = fidl::ServerEnd<fuchsia_audio_device::RingBuffer>(
                  std::move(ring_buffer_server_end))}})
        .Then([&received_callback](fidl::Result<Control::CreateRingBuffer>& result) {
          received_callback = true;
          ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        });
    RunLoopUntilIdle();
    EXPECT_TRUE(received_callback);
  }
  RunLoopUntilIdle();

  EXPECT_EQ(ObserverServer::count(), 1u);
  EXPECT_TRUE(observer->client().is_valid());
  EXPECT_FALSE(observer_fidl_error_status_);
}

}  // namespace media_audio
