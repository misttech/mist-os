// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/sync/completion.h>
#include <lib/zx/clock.h>

#include <functional>
#include <optional>
#include <vector>

#include <zxtest/zxtest.h>

#include "radar-provider-proxy.h"
#include "radar-reader-proxy.h"
#include "sdk/lib/driver/runtime/testing/runtime/dispatcher.h"

namespace radar {
namespace {

class FakeRadarDriver : public fidl::Server<fuchsia_hardware_radar::RadarBurstReaderProvider>,
                        public fidl::Server<fuchsia_hardware_radar::RadarBurstReader> {
 public:
  explicit FakeRadarDriver(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  void Bind(fidl::ServerEnd<fuchsia_hardware_radar::RadarBurstReaderProvider> server) {
    EXPECT_TRUE(fdf::RunOnDispatcherSync(dispatcher_, [&]() {
                  provider_binding_ = fidl::BindServer(dispatcher_, std::move(server), this);
                }).is_ok());
  }

  void UnbindProvider() {
    EXPECT_TRUE(fdf::RunOnDispatcherSync(dispatcher_, [&]() {
                  if (provider_binding_) {
                    provider_binding_->Close(ZX_ERR_PEER_CLOSED);
                  }
                }).is_ok());
  }

  void UnbindReader() {
    EXPECT_TRUE(fdf::RunOnDispatcherSync(dispatcher_, [&]() {
                  if (reader_binding_) {
                    reader_binding_->Close(ZX_ERR_PEER_CLOSED);
                  }
                }).is_ok());
  }

  void SendBurst() {
    async::PostTask(dispatcher_, [&]() {
      ASSERT_TRUE(reader_binding_);

      for (auto& vmo : registered_vmos_) {
        if (!vmo.locked) {
          vmo.locked = true;
          const auto burst = fuchsia_hardware_radar::RadarBurstReaderOnBurst2Request::WithBurst(
              {{vmo.vmo_id, zx::clock::get_monotonic().get()}});
          EXPECT_TRUE(fidl::SendEvent(*reader_binding_)->OnBurst2(burst).is_ok());
          return;
        }
      }

      const auto burst = fuchsia_hardware_radar::RadarBurstReaderOnBurst2Request::WithError(
          fuchsia_hardware_radar::StatusCode::kOutOfVmos);
      EXPECT_TRUE(fidl::SendEvent(*reader_binding_)->OnBurst2(burst).is_ok());
    });
  }

  void SendError(fuchsia_hardware_radar::StatusCode status) {
    async::PostTask(dispatcher_, [&, error = status]() {
      ASSERT_TRUE(reader_binding_);
      const auto burst = fuchsia_hardware_radar::RadarBurstReaderOnBurst2Request::WithError(error);
      EXPECT_TRUE(fidl::SendEvent(*reader_binding_)->OnBurst2(burst).is_ok());
    });
  }

  zx_status_t WaitForBurstsStopped() {
    return sync_completion_wait(&bursts_stopped_, ZX_TIME_INFINITE);
  }

  void set_burst_size(uint32_t burst_size) {
    EXPECT_TRUE(fdf::RunOnDispatcherSync(dispatcher_, [&, size = burst_size]() {
                  burst_size_ = size;
                }).is_ok());
  }

 private:
  struct RegisteredVmo {
    uint32_t vmo_id;
    bool locked = false;
  };

  void Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) override {
    reader_binding_ = fidl::BindServer(dispatcher_, std::move(request.server()), this);
    completer.Reply(fit::ok());
  }

  void GetBurstProperties(GetBurstPropertiesCompleter::Sync& completer) override {
    completer.Reply({burst_size_, 0});
  }

  void RegisterVmos(RegisterVmosRequest& request, RegisterVmosCompleter::Sync& completer) override {
    if (!registered_vmos_.empty()) {
      // Inject an error on the second RegisterVmos() call to test radar-proxy error handling.
      completer.Reply(fit::error(fuchsia_hardware_radar::StatusCode::kVmoAlreadyRegistered));
      return;
    }
    if (request.vmo_ids().size() != request.vmos().size()) {
      completer.Reply(fit::error(fuchsia_hardware_radar::StatusCode::kInvalidArgs));
      return;
    }

    for (const uint32_t vmo_id : request.vmo_ids()) {
      // Ignore normal registration errors, such as duplicate IDs.
      registered_vmos_.push_back(RegisteredVmo{vmo_id});
    }
    completer.Reply(fit::success());
  }

  void UnregisterVmos(UnregisterVmosRequest& request,
                      UnregisterVmosCompleter::Sync& completer) override {}

  void StartBursts(StartBurstsCompleter::Sync& completer) override {
    sync_completion_reset(&bursts_stopped_);
  }

  void StopBursts(StopBurstsCompleter::Sync& completer) override {
    completer.Reply();
    sync_completion_signal(&bursts_stopped_);
  }

  void UnlockVmo(UnlockVmoRequest& request, UnlockVmoCompleter::Sync& completer) override {
    for (auto& vmo : registered_vmos_) {
      if (vmo.vmo_id == request.vmo_id()) {
        vmo.locked = false;
        return;
      }
    }
  }

  async_dispatcher_t* const dispatcher_;
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_radar::RadarBurstReaderProvider>>
      provider_binding_;
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_radar::RadarBurstReader>> reader_binding_;
  std::vector<RegisteredVmo> registered_vmos_;
  sync_completion_t bursts_stopped_;
  uint32_t burst_size_ = 12345;
};

template <typename T>
class RadarProxyTest : public zxtest::Test, public RadarDeviceConnector {
 public:
  RadarProxyTest()
      : proxy_loop_(&kAsyncLoopConfigNoAttachToCurrentThread),
        driver_loop_(&kAsyncLoopConfigNoAttachToCurrentThread),
        fake_driver_(driver_loop_.dispatcher()),
        dut_(proxy_loop_.dispatcher(), this) {}

  void SetUp() override {
    ASSERT_OK(proxy_loop_.StartThread("Radar proxy"));
    ASSERT_OK(driver_loop_.StartThread("Radar driver"));

    zx::result endpoints =
        fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReaderProvider>();
    ASSERT_TRUE(endpoints.is_ok());

    dut_client_.Bind(std::move(endpoints->client));
    EXPECT_TRUE(fdf::RunOnDispatcherSync(proxy_loop_.dispatcher(), [&]() {
                  fidl::BindServer(proxy_loop_.dispatcher(), std::move(endpoints->server), &dut_);
                }).is_ok());

    endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReaderProvider>();
    ASSERT_TRUE(endpoints.is_ok());
  }

  void TearDown() override {
    // Make sure the proxy doesn't try to reconnect after the loop has been stopped.
    UnbindRadarDevice();

    // dut_ must outlive the loop in order to prevent FIDL callbacks from keeping dangling
    // references to them.
    proxy_loop_.Shutdown();
    driver_loop_.Shutdown();
  }

  void ConnectToRadarDevice(fidl::UnownedClientEnd<fuchsia_io::Directory> dir,
                            const std::string& path,
                            ConnectDeviceCallback connect_device) override {
    ConnectToFirstRadarDevice(std::move(connect_device));
  }

  void ConnectToFirstRadarDevice(ConnectDeviceCallback connect_device) override {
    if (!provider_connect_fail_) {
      zx::result endpoints =
          fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReaderProvider>();
      ASSERT_TRUE(endpoints.is_ok());

      fake_driver_.Bind(std::move(endpoints->server));
      // This may not succeed if we've told the radar driver to return errors for certain calls.
      connect_device(std::move(endpoints->client));
    }

    sync_completion_signal(&device_connected_);
  }

 private:
  async::Loop proxy_loop_;
  async::Loop driver_loop_;

 protected:
  static fuchsia_hardware_radar::RadarBurstReaderGetBurstPropertiesResponse kInvalidProperties() {
    return fuchsia_hardware_radar::RadarBurstReaderGetBurstPropertiesResponse(0, 0);
  }

  // Closes the RadarBurstProvider connection between radar-proxy and the radar driver, and waits
  // for radar-proxy to process the epitaph. After this call, requests to radar-proxy should either
  // succeed or fail depending on the value of provider_connect_fail_.
  void UnbindProviderAndWaitForConnectionAttempt() {
    sync_completion_reset(&device_connected_);
    UnbindRadarDevice();
    sync_completion_wait(&device_connected_, ZX_TIME_INFINITE);
  }

  // Same as above, but for RadarBurstReader.
  void UnbindReaderAndWaitForConnectionAttempt() {
    sync_completion_reset(&device_connected_);
    fake_driver_.UnbindReader();
    sync_completion_wait(&device_connected_, ZX_TIME_INFINITE);
  }

  void UnbindRadarDevice() {
    EXPECT_TRUE(fdf::RunOnDispatcherSync(proxy_loop_.dispatcher(), [&]() {
                  provider_connect_fail_ = true;
                }).is_ok());
    fake_driver_.UnbindProvider();
  }

  void AddRadarDevice() {
    EXPECT_TRUE(fdf::RunOnDispatcherSync(proxy_loop_.dispatcher(), [&]() {
                  provider_connect_fail_ = false;
                  dut_.DeviceAdded(kInvalidDir, "000");
                }).is_ok());
  }

  // Visible for testing.
  FakeRadarDriver fake_driver_;

  fidl::SyncClient<fuchsia_hardware_radar::RadarBurstReaderProvider> dut_client_;

 private:
  static constexpr fidl::UnownedClientEnd<fuchsia_io::Directory> kInvalidDir{FIDL_HANDLE_INVALID};

  T dut_;

  bool provider_connect_fail_ = false;
  sync_completion_t device_connected_;
};

using RadarReaderProxyTest = RadarProxyTest<RadarReaderProxy>;
using RadarProviderProxyTest = RadarProxyTest<RadarProviderProxy>;

TEST_F(RadarProviderProxyTest, Connect) {
  AddRadarDevice();

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  auto result = dut_client_->Connect(std::move(endpoints->server));
  EXPECT_TRUE(result.is_ok());

  fidl::SyncClient radar_client(std::move(endpoints->client));
  EXPECT_EQ(radar_client->GetBurstProperties().value_or(kInvalidProperties()).size(), 12345);
}

TEST_F(RadarProviderProxyTest, Reconnect) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  fidl::Client service_client(dut_client_.TakeClientEnd(), loop.dispatcher());

  AddRadarDevice();

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  service_client->Connect(std::move(endpoints->server)).Then([&](auto& result) {
    EXPECT_TRUE(result.is_ok());

    fidl::SyncClient radar_client(std::move(endpoints->client));
    EXPECT_EQ(radar_client->GetBurstProperties().value_or(kInvalidProperties()).size(), 12345);

    // Close the connection between the proxy and the radar driver. Calls to Connect() should
    // then be put in the request queue instead of immediately completed.
    UnbindProviderAndWaitForConnectionAttempt();

    // The connection with the radar driver itself should remain open.
    EXPECT_EQ(radar_client->GetBurstProperties().value_or(kInvalidProperties()).size(), 12345);

    endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
    ASSERT_TRUE(endpoints.is_ok());

    // Issue a connect request to be completed when a new device shows up.
    service_client->Connect(std::move(endpoints->server)).Then([&](auto& result) {
      EXPECT_TRUE(result.is_ok());

      // The proxy connected to the new device and completed our request. We should now have a new
      // connection to the underlying driver.
      fidl::SyncClient new_radar_client(std::move(endpoints->client));
      EXPECT_EQ(new_radar_client->GetBurstProperties().value_or(kInvalidProperties()).size(),
                12345);

      loop.Quit();
    });

    // Signal that a new device was added to /dev/class/radar. The proxy should connect to the new
    // device and complete our previous request.
    AddRadarDevice();
  });

  loop.Run();
}

TEST_F(RadarProviderProxyTest, DelayedConnect) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  fidl::Client service_client(dut_client_.TakeClientEnd(), loop.dispatcher());

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  // Attempt to connect before a radar device is available. Our request should be put in a queue and
  // completed when a device is added.
  service_client->Connect(std::move(endpoints->server)).Then([&](auto& result) {
    EXPECT_TRUE(result.is_ok());
  });

  fidl::Client radar_client(std::move(endpoints->client), loop.dispatcher());

  uint32_t burst_size = 0;
  radar_client->GetBurstProperties().Then([&](auto& result) {
    burst_size = result.is_ok() ? result->size() : 0;
    loop.Quit();
  });

  loop.RunUntilIdle();

  // Signal to the proxy that a device was added. This should cause it to process any connect
  // requests that came in before this point.
  AddRadarDevice();

  // The loop will be stopped after the burst size call completes.
  loop.Run();

  EXPECT_EQ(burst_size, 12345);
}

TEST_F(RadarReaderProxyTest, Connect) {
  AddRadarDevice();

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  auto result = dut_client_->Connect(std::move(endpoints->server));
  EXPECT_TRUE(result.is_ok());

  fidl::SyncClient radar_client(std::move(endpoints->client));
  EXPECT_EQ(radar_client->GetBurstProperties().value_or(kInvalidProperties()).size(), 12345);
}

TEST_F(RadarReaderProxyTest, Reconnect) {
  AddRadarDevice();

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  fidl::Client service_client(dut_client_.TakeClientEnd(), loop.dispatcher());

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  service_client->Connect(std::move(endpoints->server)).Then([&](auto& result) {
    EXPECT_TRUE(result.is_ok());
  });

  fidl::SyncClient radar_client(std::move(endpoints->client));
  EXPECT_EQ(radar_client->GetBurstProperties().value_or(kInvalidProperties()).size(), 12345);

  // Close the Provider connection between the proxy and the radar driver. Everything should still
  // work as long as the Reader connection remains open.
  UnbindRadarDevice();

  // Verify that Connect() still works.
  endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  service_client->Connect(std::move(endpoints->server)).Then([&](auto& result) {
    EXPECT_TRUE(result.is_ok());
  });

  fidl::SyncClient new_radar_client(std::move(endpoints->client));
  EXPECT_EQ(new_radar_client->GetBurstProperties().value_or(kInvalidProperties()).size(), 12345);

  // Verify that the original connection still works.
  EXPECT_EQ(radar_client->GetBurstProperties().value_or(kInvalidProperties()).size(), 12345);

  UnbindReaderAndWaitForConnectionAttempt();

  // Client connections were closed, new requests should now fail.
  EXPECT_FALSE(radar_client->GetBurstProperties().is_ok());

  endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  new_radar_client = fidl::SyncClient(std::move(endpoints->client));

  // Send a connect request while there is no radar driver. It should be completed (and the
  // connection usable) after a new radar driver is added.
  service_client->Connect(std::move(endpoints->server)).Then([&](auto& result) {
    EXPECT_TRUE(result.is_ok());
  });

  // Signal that a new device was added to /dev/class/radar.
  AddRadarDevice();

  // Verify that the previous connect request suceeded.
  EXPECT_EQ(new_radar_client->GetBurstProperties().value_or(kInvalidProperties()).size(), 12345);

  // Subsequent connect requests should be immediately fulfilled.
  endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  service_client->Connect(std::move(endpoints->server)).Then([&](auto& result) {
    EXPECT_TRUE(result.is_ok());
    loop.Quit();
  });

  new_radar_client = fidl::SyncClient(std::move(endpoints->client));
  EXPECT_EQ(new_radar_client->GetBurstProperties().value_or(kInvalidProperties()).size(), 12345);

  // Ensure that all of our previous async requests got replies.
  loop.Run();
}

TEST_F(RadarReaderProxyTest, DelayedConnect) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  fidl::Client service_client(dut_client_.TakeClientEnd(), loop.dispatcher());

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  // Attempt to connect before a radar device is available. Our request should be put in a queue and
  // completed when a device is added.
  service_client->Connect(std::move(endpoints->server)).Then([&](auto& result) {
    EXPECT_TRUE(result.is_ok());
  });

  fidl::Client radar_client(std::move(endpoints->client), loop.dispatcher());

  uint32_t burst_size = 0;
  radar_client->GetBurstProperties().Then([&](auto& result) {
    burst_size = result.is_ok() ? result->size() : 0;
    loop.Quit();
  });

  loop.RunUntilIdle();

  // Signal to the proxy that a device was added. This should cause it to process any connect
  // requests that came in before this point.
  AddRadarDevice();

  // The loop will be stopped after the burst size call completes.
  loop.Run();

  EXPECT_EQ(burst_size, 12345);
}

TEST_F(RadarReaderProxyTest, RegisterVmosError) {
  AddRadarDevice();

  struct : fidl::AsyncEventHandler<fuchsia_hardware_radar::RadarBurstReader> {
    async::Loop loop{&kAsyncLoopConfigNeverAttachToThread};
    fidl::Client<fuchsia_hardware_radar::RadarBurstReader> client;
    void on_fidl_error(fidl::UnbindInfo info) override { loop.Quit(); }
    void Bind(fidl::ClientEnd<fuchsia_hardware_radar::RadarBurstReader> client_end) {
      client.Bind(std::move(client_end), loop.dispatcher(), this);
    }
  } radar_client;

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  {
    auto result = dut_client_->Connect(std::move(endpoints->server));
    EXPECT_TRUE(result.is_ok());
  }

  radar_client.Bind(std::move(endpoints->client));

  {
    std::vector<zx::vmo> vmos;
    EXPECT_OK(zx::vmo::create(12345, 0, &vmos.emplace_back()));
    radar_client.client->RegisterVmos({std::vector<uint32_t>{1}, std::move(vmos)})
        .Then([&](auto& result) { EXPECT_TRUE(result.is_ok()); });
  }

  {
    std::vector<zx::vmo> vmos;
    EXPECT_OK(zx::vmo::create(12345, 0, &vmos.emplace_back()));
    // The call to radar-proxy should succeed, however the call from radar-proxy to the underlying
    // driver should fail. When that happens radar-proxy should send an epitaph to the client.
    radar_client.client->RegisterVmos({std::vector<uint32_t>{2}, std::move(vmos)})
        .Then([&](auto& result) { EXPECT_TRUE(result.is_ok()); });
  }

  // Run until on_fidl_error receives the epitaph and stops us.
  radar_client.loop.Run();
}

TEST_F(RadarReaderProxyTest, DeviceWithInvalidBurstSizeIsRejected) {
  AddRadarDevice();

  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  fidl::Client service_client(dut_client_.TakeClientEnd(), loop.dispatcher());

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  fidl::SyncClient radar_client(std::move(endpoints->client));

  service_client->Connect(std::move(endpoints->server)).Then([&](auto& result) {
    EXPECT_TRUE(result.is_ok());
    EXPECT_EQ(radar_client->GetBurstProperties().value_or(kInvalidProperties()).size(), 12345);

    UnbindRadarDevice();
    UnbindReaderAndWaitForConnectionAttempt();

    zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
    ASSERT_TRUE(endpoints.is_ok());

    radar_client = fidl::SyncClient(std::move(endpoints->client));

    // With no connected device this request should be put in the queue and completed later.
    service_client->Connect(std::move(endpoints->server)).Then([&](auto& result) {
      EXPECT_TRUE(result.is_ok());
      EXPECT_EQ(radar_client->GetBurstProperties().value_or(kInvalidProperties()).size(), 12345);
      loop.Quit();
    });

    // The burst size reported by this device does not match the original value, so it should be
    // ignored.
    fake_driver_.set_burst_size(12346);
    AddRadarDevice();

    // This device matches the original, so it should be accepted and used to complete the connect
    // request.
    fake_driver_.set_burst_size(12345);
    AddRadarDevice();
  });

  loop.Run();
}

class RadarReaderProxyOnBurstTest : public RadarReaderProxyTest {
 public:
  RadarReaderProxyOnBurstTest() : loop_(&kAsyncLoopConfigNeverAttachToThread) {}

  void SetUp() override {
    RadarReaderProxyTest::SetUp();

    AddRadarDevice();

    const std::function burst_callback([&](uint32_t burst_count) { BurstCallback(burst_count); });

    radar_clients_.reserve(4);
    for (uint32_t i = 0; i < 4; i++) {
      zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
      ASSERT_TRUE(endpoints.is_ok());

      EXPECT_TRUE(dut_client_->Connect(std::move(endpoints->server)).is_ok());

      radar_clients_.emplace_back(std::move(endpoints->client), loop_.dispatcher(),
                                  // Make the last client not unlock VMOs to test error cases.
                                  i == 3 ? std::function<void(uint32_t)>{} : burst_callback);

      std::vector<zx::vmo> vmos;
      EXPECT_OK(zx::vmo::create(12345, 0, &vmos.emplace_back()));
      EXPECT_OK(zx::vmo::create(12345, 0, &vmos.emplace_back()));
      EXPECT_OK(zx::vmo::create(12345, 0, &vmos.emplace_back()));
      EXPECT_OK(zx::vmo::create(12345, 0, &vmos.emplace_back()));
      EXPECT_OK(zx::vmo::create(12345, 0, &vmos.emplace_back()));
      radar_clients_.back()
          ->RegisterVmos({std::vector<uint32_t>{1, 2, 3, 4, 5}, std::move(vmos)})
          .Then([&](auto& result) { EXPECT_TRUE(result.is_ok()); });
    }
  }

 protected:
  // A client wrapper that supports receiving OnBurst() events. If no burst callback is specified
  // then VMOs will not be unlocked.
  struct BurstReaderClient : fidl::AsyncEventHandler<fuchsia_hardware_radar::RadarBurstReader> {
   public:
    BurstReaderClient(fidl::ClientEnd<fuchsia_hardware_radar::RadarBurstReader> client_end,
                      async_dispatcher_t* dispatcher, std::function<void(uint32_t)> callback)
        : client(std::move(client_end), dispatcher, this), burst_callback(std::move(callback)) {}

    void OnBurst(fidl::Event<fuchsia_hardware_radar::RadarBurstReader::OnBurst>& event) override {
      if (event.result().response()) {
        burst_count++;
        if (burst_callback) {
          EXPECT_TRUE(client->UnlockVmo(event.result().response()->burst().vmo_id()).is_ok());
          burst_callback(burst_count);
        }
      } else {
        error_status = event.result().err().value();
      }
    }

    fidl::Client<fuchsia_hardware_radar::RadarBurstReader>& operator->() { return client; }

    fidl::Client<fuchsia_hardware_radar::RadarBurstReader> client;
    uint32_t burst_count = 0;
    fuchsia_hardware_radar::StatusCode error_status = fuchsia_hardware_radar::StatusCode::kSuccess;

   private:
    const std::function<void(uint32_t)> burst_callback;
  };

  std::vector<BurstReaderClient> radar_clients_;
  async::Loop loop_;

 private:
  // Assume that there are always this many connected clients with bursts started. A counter will be
  // compared to this value to ensure that the driver doesn't send a new burst until all clients
  // have unlocked their VMOs.
  static constexpr uint32_t kClientsWithBurstsStarted = 2;
  // Keep running until all clients receive this many bursts, then quit the loop.
  static constexpr uint32_t kReceiveBurstCount = 10;

  void BurstCallback(uint32_t burst_count) {
    static uint32_t client_lockstep_counter = 0;
    // Increment the counter until all clients have received this burst. At that point we can either
    // send another burst, or stop the loop.
    if (++client_lockstep_counter == kClientsWithBurstsStarted) {
      client_lockstep_counter = 0;
      if (burst_count == kReceiveBurstCount) {
        loop_.Quit();
      } else {
        fake_driver_.SendBurst();
      }
    }
  }
};

TEST_F(RadarReaderProxyOnBurstTest, OnlyStartedClientsReceiveBursts) {
  // Clients 0 and 1 start bursts. Verify that client 2 does not receive bursts.

  EXPECT_TRUE(radar_clients_[0]->StartBursts().is_ok());
  EXPECT_TRUE(radar_clients_[1]->StartBursts().is_ok());

  // Use the GetBurstProperties() reply as a trigger to start sending bursts. At that point we know
  // that radar-proxy has already processed our StartBursts() call, so any new bursts will be
  // forwarded to clients.
  radar_clients_[0]->GetBurstProperties().Then([&](auto& result) { fake_driver_.SendBurst(); });

  loop_.Run();

  EXPECT_EQ(radar_clients_[0].burst_count, 10);
  EXPECT_EQ(radar_clients_[1].burst_count, 10);
  EXPECT_EQ(radar_clients_[2].burst_count, 0);
}

TEST_F(RadarReaderProxyOnBurstTest, DriverErrorsSentToStartedClients) {
  // Clients 1 and 2 start bursts. The driver reports a burst error, which gets sent to only to
  // these clients.

  EXPECT_TRUE(radar_clients_[1]->StartBursts().is_ok());
  EXPECT_TRUE(radar_clients_[2]->StartBursts().is_ok());

  radar_clients_[0]->GetBurstProperties().Then([&](auto& result) {
    fake_driver_.SendError(fuchsia_hardware_radar::StatusCode::kSensorError);
    fake_driver_.SendBurst();
  });

  loop_.Run();

  EXPECT_EQ(radar_clients_[0].burst_count, 0);
  EXPECT_EQ(radar_clients_[1].burst_count, 10);
  EXPECT_EQ(radar_clients_[2].burst_count, 10);

  EXPECT_EQ(radar_clients_[0].error_status, fuchsia_hardware_radar::StatusCode::kSuccess);
  EXPECT_EQ(radar_clients_[1].error_status, fuchsia_hardware_radar::StatusCode::kSensorError);
  EXPECT_EQ(radar_clients_[2].error_status, fuchsia_hardware_radar::StatusCode::kSensorError);
}

TEST_F(RadarReaderProxyOnBurstTest, OnlyOffendingClientReceivesOutOfVmosError) {
  // Start bursts for client 3, which we configured to not unlock VMOs. Client 3 should get a burst
  // for every registered VMO (five), then a kOutOfVmos error. Other clients should continue to get
  // bursts delivered like normal.

  EXPECT_TRUE(radar_clients_[1]->StartBursts().is_ok());
  EXPECT_TRUE(radar_clients_[2]->StartBursts().is_ok());
  EXPECT_TRUE(radar_clients_[3]->StartBursts().is_ok());

  radar_clients_[0]->GetBurstProperties().Then([&](auto& result) { fake_driver_.SendBurst(); });

  loop_.Run();

  EXPECT_EQ(radar_clients_[1].burst_count, 10);
  EXPECT_EQ(radar_clients_[2].burst_count, 10);
  EXPECT_EQ(radar_clients_[3].burst_count, 5);

  EXPECT_EQ(radar_clients_[1].error_status, fuchsia_hardware_radar::StatusCode::kSuccess);
  EXPECT_EQ(radar_clients_[2].error_status, fuchsia_hardware_radar::StatusCode::kSuccess);
  EXPECT_EQ(radar_clients_[3].error_status, fuchsia_hardware_radar::StatusCode::kOutOfVmos);
}

TEST_F(RadarReaderProxyOnBurstTest, DisconnectedClientStopsReceivingBursts) {
  // Client 1 disconnects, now clients 0 and 2 get bursts.

  EXPECT_TRUE(radar_clients_[0]->StartBursts().is_ok());
  EXPECT_TRUE(radar_clients_[1]->StartBursts().is_ok());
  EXPECT_TRUE(radar_clients_[2]->StartBursts().is_ok());

  radar_clients_[1].client = fidl::Client<fuchsia_hardware_radar::RadarBurstReader>();

  radar_clients_[0]->GetBurstProperties().Then([&](auto& result) { fake_driver_.SendBurst(); });

  loop_.Run();

  EXPECT_EQ(radar_clients_[0].burst_count, 10);
  EXPECT_EQ(radar_clients_[1].burst_count, 0);
  EXPECT_EQ(radar_clients_[2].burst_count, 10);
}

TEST_F(RadarReaderProxyOnBurstTest, BurstsStoppedAfterAllClientsDisconnect) {
  // All clients disconnect, bursts should be stopped.

  EXPECT_TRUE(radar_clients_[0]->StartBursts().is_ok());
  EXPECT_TRUE(radar_clients_[1]->StartBursts().is_ok());
  EXPECT_TRUE(radar_clients_[2]->StartBursts().is_ok());

  radar_clients_[0]->GetBurstProperties().Then([&](auto& result) { fake_driver_.SendBurst(); });

  radar_clients_.clear();

  EXPECT_OK(fake_driver_.WaitForBurstsStopped());
}

TEST_F(RadarReaderProxyOnBurstTest, StoppedClientStopsReceivingBursts) {
  // A client that stops bursts should not receive any more bursts.

  EXPECT_TRUE(radar_clients_[0]->StartBursts().is_ok());
  EXPECT_TRUE(radar_clients_[1]->StartBursts().is_ok());

  radar_clients_[0]->GetBurstProperties().Then([&](auto& result) { fake_driver_.SendBurst(); });

  loop_.Run();
  loop_.ResetQuit();

  EXPECT_EQ(radar_clients_[0].burst_count, 10);
  EXPECT_EQ(radar_clients_[1].burst_count, 10);
  EXPECT_EQ(radar_clients_[2].burst_count, 0);

  radar_clients_[0].burst_count = 0;
  radar_clients_[1].burst_count = 0;
  radar_clients_[2].burst_count = 0;

  radar_clients_[1]->StopBursts().Then([&](auto& result) { EXPECT_TRUE(result.is_ok()); });
  EXPECT_TRUE(radar_clients_[2]->StartBursts().is_ok());

  radar_clients_[0]->GetBurstProperties().Then([&](auto& result) { fake_driver_.SendBurst(); });

  loop_.Run();

  EXPECT_EQ(radar_clients_[0].burst_count, 10);
  EXPECT_EQ(radar_clients_[1].burst_count, 0);
  EXPECT_EQ(radar_clients_[2].burst_count, 10);
}

}  // namespace
}  // namespace radar
