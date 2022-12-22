// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/media/audio/services/device_registry/observer_server.h"

#include <fidl/fuchsia.audio.device/cpp/fidl.h>
#include <fidl/fuchsia.audio.device/cpp/natural_types.h>
#include <fidl/fuchsia.hardware.audio/cpp/fidl.h>
#include <zircon/errors.h>

#include <memory>
#include <optional>

#include <gtest/gtest.h>

#include "src/media/audio/services/device_registry/adr_server_unittest_base.h"
#include "src/media/audio/services/device_registry/testing/fake_audio_driver.h"

namespace media_audio {
namespace {

namespace fidl_device = fuchsia_audio_device;

class ObserverServerTest : public AudioDeviceRegistryServerTestBase,
                           public fidl::AsyncEventHandler<fidl_device::Observer> {
 protected:
  static inline const fuchsia_audio::Format kDefaultRingBufferFormat{{
      .sample_type = fuchsia_audio::SampleType::kInt16,
      .channel_count = 2,
      .frames_per_second = 48000,
  }};

  std::unique_ptr<FakeAudioDriver> CreateAndEnableDriverWithDefaults();
  std::pair<fidl::Client<fidl_device::Registry>, std::shared_ptr<RegistryServer>>
  CreateRegistryServer();
  std::optional<TokenId> WaitForAddedDeviceTokenId(fidl::Client<fidl_device::Registry>& reg_client);
  std::optional<TokenId> WaitForRemovedDeviceTokenId(
      fidl::Client<fidl_device::Registry>& reg_client);
  std::pair<fidl::Client<fidl_device::ControlCreator>, std::shared_ptr<ControlCreatorServer>>
  CreateControlCreatorServer();
  fidl::Client<fidl_device::Control> ConnectToControl(
      fidl::Client<fidl_device::ControlCreator>& ctl_creator_client, TokenId token_id);
  std::pair<fidl::Client<fidl_device::RingBuffer>, fidl::ServerEnd<fidl_device::RingBuffer>>
  CreateRingBufferClient();
  fidl::Client<fidl_device::Observer> ConnectToObserver(
      fidl::Client<fidl_device::Registry>& reg_client, TokenId token_id);
  // Invoked when the underlying driver disconnects its StreamConfig.
  void on_fidl_error(fidl::UnbindInfo error) override;

  std::optional<zx_status_t> fidl_error_status_;
};

std::unique_ptr<FakeAudioDriver> ObserverServerTest::CreateAndEnableDriverWithDefaults() {
  EXPECT_EQ(dispatcher(), test_loop().dispatcher());
  zx::channel server_end, client_end;
  EXPECT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
  auto fake_driver =
      std::make_unique<FakeAudioDriver>(std::move(server_end), std::move(client_end), dispatcher());

  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fidl_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  return fake_driver;
}

std::pair<fidl::Client<fidl_device::Registry>, std::shared_ptr<RegistryServer>>
ObserverServerTest::CreateRegistryServer() {
  auto [client_end, server_end] = CreateNaturalAsyncClientOrDie<fidl_device::Registry>();
  auto server = adr_service_->CreateRegistryServer(std::move(server_end));
  auto client = fidl::Client<fidl_device::Registry>(std::move(client_end), dispatcher());
  return std::make_pair(std::move(client), server);
}

std::optional<TokenId> ObserverServerTest::WaitForAddedDeviceTokenId(
    fidl::Client<fidl_device::Registry>& reg_client) {
  std::optional<TokenId> added_device_id;
  reg_client->WatchDevicesAdded().Then(
      [&added_device_id](fidl::Result<fidl_device::Registry::WatchDevicesAdded>& result) mutable {
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
    fidl::Client<fidl_device::Registry>& reg_client) {
  std::optional<TokenId> removed_device_id;
  reg_client->WatchDeviceRemoved().Then(
      [&removed_device_id](
          fidl::Result<fidl_device::Registry::WatchDeviceRemoved>& result) mutable {
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->token_id());
        removed_device_id = *result->token_id();
      });
  RunLoopUntilIdle();
  return removed_device_id;
}

std::pair<fidl::Client<fidl_device::ControlCreator>, std::shared_ptr<ControlCreatorServer>>
ObserverServerTest::CreateControlCreatorServer() {
  auto [client_end, server_end] = CreateNaturalAsyncClientOrDie<fidl_device::ControlCreator>();
  auto server = adr_service_->CreateControlCreatorServer(std::move(server_end));
  auto client = fidl::Client<fidl_device::ControlCreator>(std::move(client_end), dispatcher());
  return std::make_pair(std::move(client), server);
}

fidl::Client<fidl_device::Control> ObserverServerTest::ConnectToControl(
    fidl::Client<fidl_device::ControlCreator>& ctl_creator_client, TokenId token_id) {
  auto [control_client_end, control_server_end] =
      CreateNaturalAsyncClientOrDie<fidl_device::Control>();
  auto control_client = fidl::Client<fidl_device::Control>(
      fidl::ClientEnd<fidl_device::Control>(std::move(control_client_end)), dispatcher());
  bool received_callback = false;
  ctl_creator_client
      ->Create({{
          .token_id = token_id,
          .control_server = fidl::ServerEnd<fidl_device::Control>(std::move(control_server_end)),
      }})
      .Then([&received_callback](fidl::Result<fidl_device::ControlCreator::Create>& result) {
        EXPECT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_TRUE(control_client.is_valid());
  return control_client;
}

std::pair<fidl::Client<fidl_device::RingBuffer>, fidl::ServerEnd<fidl_device::RingBuffer>>
ObserverServerTest::CreateRingBufferClient() {
  auto [ring_buffer_client_end, ring_buffer_server_end] =
      CreateNaturalAsyncClientOrDie<fidl_device::RingBuffer>();
  auto ring_buffer_client = fidl::Client<fidl_device::RingBuffer>(
      fidl::ClientEnd<fidl_device::RingBuffer>(std::move(ring_buffer_client_end)), dispatcher());
  return std::make_pair(std::move(ring_buffer_client), std::move(ring_buffer_server_end));
}

fidl::Client<fidl_device::Observer> ObserverServerTest::ConnectToObserver(
    fidl::Client<fidl_device::Registry>& reg_client, TokenId token_id) {
  auto [observer_client_end, observer_server_end] =
      CreateNaturalAsyncClientOrDie<fidl_device::Observer>();
  auto observer_client = fidl::Client<fidl_device::Observer>(
      fidl::ClientEnd<fidl_device::Observer>(std::move(observer_client_end)), dispatcher(), this);
  bool received_callback = false;
  reg_client
      ->CreateObserver({{
          .token_id = token_id,
          .observer_server = fidl::ServerEnd<fidl_device::Observer>(std::move(observer_server_end)),
      }})
      .Then([&received_callback](fidl::Result<fidl_device::Registry::CreateObserver>& result) {
        ASSERT_TRUE(result.is_ok());
        received_callback = true;
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);
  EXPECT_TRUE(observer_client.is_valid());
  return observer_client;
}

// Invoked when the underlying driver disconnects its StreamConfig.
void ObserverServerTest::on_fidl_error(fidl::UnbindInfo error) {
  fidl_error_status_ = error.status();
  if (fidl_error_status_ != ZX_OK && fidl_error_status_ != ZX_ERR_PEER_CLOSED) {
    FX_LOGS(WARNING) << __func__ << ":" << error;
  } else {
    FX_LOGS(INFO) << __func__ << ":" << error;
  }
}

TEST_F(ObserverServerTest, CleanClientDrop) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto wrapper = std::make_unique<TestServerAndNaturalAsyncClient<ObserverServer>>(
      test_loop(), server_thread_, *adr_service_->devices().begin());
  RunLoopUntilIdle();
  ASSERT_EQ(ObserverServer::count(), 1u);

  wrapper->client() = fidl::Client<fidl_device::Observer>();
  RunLoopUntilIdle();
  EXPECT_TRUE(wrapper->server().WaitForShutdown(zx::sec(1)));
}

TEST_F(ObserverServerTest, CleanServerShutdown) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  auto wrapper = std::make_unique<TestServerAndNaturalAsyncClient<ObserverServer>>(
      test_loop(), server_thread_, *adr_service_->devices().begin());
  RunLoopUntilIdle();
  ASSERT_EQ(ObserverServer::count(), 1u);

  wrapper->server().Shutdown();
  RunLoopUntilIdle();
  EXPECT_TRUE(wrapper->server().WaitForShutdown(zx::sec(1)));
}

TEST_F(ObserverServerTest, Creation) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  auto [reg_client, reg_server] = CreateRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(reg_client);
  ASSERT_TRUE(added_device_id);

  auto observer_client = ConnectToObserver(reg_client, *added_device_id);

  reg_client = fidl::Client<fidl_device::Registry>();
  RunLoopUntilIdle();
  reg_server->WaitForShutdown(zx::sec(1));

  EXPECT_FALSE(fidl_error_status_);
}

TEST_F(ObserverServerTest, ObservedDeviceRemoved) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  auto [reg_client, reg_server] = CreateRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(reg_client);
  ASSERT_TRUE(added_device_id);

  auto observer_client = ConnectToObserver(reg_client, *added_device_id);

  fake_driver->DropStreamConfig();
  RunLoopUntilIdle();
  auto removed_device_id = WaitForRemovedDeviceTokenId(reg_client);
  ASSERT_TRUE(removed_device_id);

  EXPECT_EQ(ObserverServer::count(), 0u);
  EXPECT_TRUE(fidl_error_status_);
  EXPECT_EQ(*fidl_error_status_, ZX_ERR_PEER_CLOSED);

  reg_client = fidl::Client<fidl_device::Registry>();
  RunLoopUntilIdle();
  reg_server->WaitForShutdown(zx::sec(1));
}

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
      adr_service_, dispatcher(), "Test output name", fidl_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  auto [reg_client, reg_server] = CreateRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(reg_client);
  ASSERT_TRUE(added_device_id);

  auto observer_client = ConnectToObserver(reg_client, *added_device_id);
  bool received_callback = false;
  observer_client->WatchGainState().Then(
      [&received_callback, kGainDb](fidl::Result<fidl_device::Observer::WatchGainState>& result) {
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

  reg_client = fidl::Client<fidl_device::Registry>();
  RunLoopUntilIdle();
  reg_server->WaitForShutdown(zx::sec(1));
}

TEST_F(ObserverServerTest, GainChange) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  auto [reg_client, reg_server] = CreateRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(reg_client);
  ASSERT_TRUE(added_device_id);

  auto observer_client = ConnectToObserver(reg_client, *added_device_id);
  bool received_callback = false;
  observer_client->WatchGainState().Then(
      [&received_callback](fidl::Result<fidl_device::Observer::WatchGainState>& result) {
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
  observer_client->WatchGainState().Then(
      [&received_callback, kGainDb](fidl::Result<fidl_device::Observer::WatchGainState>& result) {
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

  reg_client = fidl::Client<fidl_device::Registry>();
  RunLoopUntilIdle();
  reg_server->WaitForShutdown(zx::sec(1));
}

TEST_F(ObserverServerTest, InitialPlugState) {
  zx::channel server_end, client_end;
  EXPECT_EQ(ZX_OK, zx::channel::create(0, &server_end, &client_end));
  auto fake_driver =
      std::make_unique<FakeAudioDriver>(std::move(server_end), std::move(client_end), dispatcher());
  auto initial_plug_time = zx::clock::get_monotonic();
  fake_driver->InjectPlugChange(false, initial_plug_time);
  RunLoopUntilIdle();
  adr_service_->AddDevice(Device::Create(
      adr_service_, dispatcher(), "Test output name", fidl_device::DeviceType::kOutput,
      fidl::ClientEnd<fuchsia_hardware_audio::StreamConfig>(fake_driver->Enable())));
  RunLoopUntilIdle();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  auto [reg_client, reg_server] = CreateRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(reg_client);
  ASSERT_TRUE(added_device_id);

  auto observer_client = ConnectToObserver(reg_client, *added_device_id);
  bool received_callback = false;
  observer_client->WatchPlugState().Then(
      [&received_callback,
       initial_plug_time](fidl::Result<fidl_device::Observer::WatchPlugState>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->state());
        EXPECT_EQ(*result->state(), fidl_device::PlugState::kUnplugged);
        ASSERT_TRUE(result->plug_time());
        EXPECT_EQ(*result->plug_time(), initial_plug_time.get());
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  reg_client = fidl::Client<fidl_device::Registry>();
  RunLoopUntilIdle();
  reg_server->WaitForShutdown(zx::sec(1));
}

TEST_F(ObserverServerTest, PlugChange) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  auto [reg_client, reg_server] = CreateRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(reg_client);
  ASSERT_TRUE(added_device_id);
  auto time_of_plug_change = zx::clock::get_monotonic();

  auto observer_client = ConnectToObserver(reg_client, *added_device_id);
  bool received_callback = false;
  observer_client->WatchPlugState().Then(
      [&received_callback,
       time_of_plug_change](fidl::Result<fidl_device::Observer::WatchPlugState>& result) {
        FX_LOGS(INFO) << "Received callback 1";
        received_callback = true;
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->state());
        EXPECT_EQ(*result->state(), fidl_device::PlugState::kPlugged);
        ASSERT_TRUE(result->plug_time());
        EXPECT_LT(*result->plug_time(), time_of_plug_change.get());
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  received_callback = false;
  observer_client->WatchPlugState().Then(
      [&received_callback,
       time_of_plug_change](fidl::Result<fidl_device::Observer::WatchPlugState>& result) {
        FX_LOGS(INFO) << "Received callback 2";
        received_callback = true;
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->state());
        EXPECT_EQ(*result->state(), fidl_device::PlugState::kUnplugged);
        ASSERT_TRUE(result->plug_time());
        EXPECT_EQ(*result->plug_time(), time_of_plug_change.get());
      });
  RunLoopUntilIdle();
  EXPECT_FALSE(received_callback);

  fake_driver->InjectPlugChange(false, time_of_plug_change);
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  reg_client = fidl::Client<fidl_device::Registry>();
  RunLoopUntilIdle();
  reg_server->WaitForShutdown(zx::sec(1));
}

TEST_F(ObserverServerTest, GetReferenceClock) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  ASSERT_EQ(adr_service_->devices().size(), 1u);
  ASSERT_EQ(adr_service_->unhealthy_devices().size(), 0u);

  auto [reg_client, reg_server] = CreateRegistryServer();
  ASSERT_EQ(RegistryServer::count(), 1u);

  auto added_device_id = WaitForAddedDeviceTokenId(reg_client);
  ASSERT_TRUE(added_device_id);

  auto observer_client = ConnectToObserver(reg_client, *added_device_id);
  bool received_callback = false;
  observer_client->GetReferenceClock().Then(
      [&received_callback](fidl::Result<fidl_device::Observer::GetReferenceClock>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok());
        ASSERT_TRUE(result->reference_clock());
        zx::clock clock = std::move(*result->reference_clock());
        EXPECT_TRUE(clock.is_valid());
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  reg_client = fidl::Client<fidl_device::Registry>();
  RunLoopUntilIdle();
  reg_server->WaitForShutdown(zx::sec(1));
}

TEST_F(ObserverServerTest, ObserverDoesNotDropIfDriverRingBufferDrops) {
  auto fake_driver = CreateAndEnableDriverWithDefaults();
  fake_driver->AllocateRingBuffer(8192);

  auto [control_creator_client, control_creator_server] = CreateControlCreatorServer();
  auto [registry_client, registry_server] = CreateRegistryServer();
  auto added_device_id = WaitForAddedDeviceTokenId(registry_client);

  ASSERT_TRUE(added_device_id);
  auto control_client = ConnectToControl(control_creator_client, *added_device_id);
  auto observer_client = ConnectToObserver(registry_client, *added_device_id);

  control_creator_server->Shutdown();
  registry_server->Shutdown();
  RunLoopUntilIdle();

  auto [ring_buffer_client, ring_buffer_server_end] = CreateRingBufferClient();
  bool received_callback = false;
  control_client
      ->CreateRingBuffer(
          {{.options = fidl_device::RingBufferOptions{{.format = kDefaultRingBufferFormat,
                                                       .ring_buffer_min_bytes = 2000}},
            .ring_buffer_server =
                fidl::ServerEnd<fidl_device::RingBuffer>(std::move(ring_buffer_server_end))}})
      .Then([&received_callback](fidl::Result<fidl_device::Control::CreateRingBuffer>& result) {
        received_callback = true;
        ASSERT_TRUE(result.is_ok()) << result.error_value().FormatDescription();
      });
  RunLoopUntilIdle();
  EXPECT_TRUE(received_callback);

  fake_driver->DropRingBuffer();
  RunLoopUntilIdle();

  EXPECT_EQ(ObserverServer::count(), 1u);
  EXPECT_FALSE(fidl_error_status_);

  EXPECT_TRUE(control_creator_server->WaitForShutdown(zx::sec(1)));
  EXPECT_TRUE(registry_server->WaitForShutdown(zx::sec(1)));
  ring_buffer_client = fidl::Client<fidl_device::RingBuffer>();
  control_client = fidl::Client<fidl_device::Control>();
}

}  // namespace
}  // namespace media_audio
