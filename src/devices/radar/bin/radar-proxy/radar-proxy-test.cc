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
          vmo.vmo.write(&kRealRadarBurstMarker, 0, sizeof(kRealRadarBurstMarker));
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
    async::PostTask(dispatcher_, [&, size = burst_size]() { burst_size_ = size; });
  }

 private:
  // We write this marker to the first byte of each burst we send, while injection tests write any
  // other value. This way clients can determine whether received bursts came from the driver or
  // were injected.
  static constexpr uint8_t kRealRadarBurstMarker = 0xff;

  struct RegisteredVmo {
    uint32_t vmo_id;
    zx::vmo vmo;
    bool locked = false;
  };

  void Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) override {
    reader_binding_ = fidl::BindServer(dispatcher_, std::move(request.server()), this);
    completer.Reply(fit::ok());
  }

  void GetBurstProperties(GetBurstPropertiesCompleter::Sync& completer) override {
    completer.Reply({burst_size_, zx::usec(1).to_nsecs()});
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

    for (size_t i = 0; i < request.vmos().size(); i++) {
      // Ignore normal registration errors, such as duplicate IDs.
      registered_vmos_.push_back(RegisteredVmo{request.vmo_ids()[i], std::move(request.vmos()[i])});
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

  void BindInjector(fidl::ServerEnd<fuchsia_hardware_radar::RadarBurstInjector> server_end) {
    dut_.BindInjector(std::move(server_end));
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

// A client wrapper that supports receiving OnBurst() events. If no burst callback is specified then
// VMOs will not be unlocked.
struct BurstReaderClient : fidl::AsyncEventHandler<fuchsia_hardware_radar::RadarBurstReader> {
 public:
  BurstReaderClient(fidl::ClientEnd<fuchsia_hardware_radar::RadarBurstReader> client_end,
                    async_dispatcher_t* dispatcher, std::function<void(uint32_t)> callback)
      : client(std::move(client_end), dispatcher, this), burst_callback(std::move(callback)) {}

  void OnBurst(fidl::Event<fuchsia_hardware_radar::RadarBurstReader::OnBurst>& event) override {
    if (event.result().response()) {
      burst_count++;
      if (burst_callback) {
        burst_callback(event.result().response()->burst().vmo_id());
        EXPECT_TRUE(client->UnlockVmo(event.result().response()->burst().vmo_id()).is_ok());
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

class RadarReaderProxyOnBurstTest : public RadarReaderProxyTest {
 public:
  RadarReaderProxyOnBurstTest() : loop_(&kAsyncLoopConfigNeverAttachToThread) {}

  void SetUp() override {
    RadarReaderProxyTest::SetUp();

    AddRadarDevice();

    const std::function burst_callback([&](uint32_t) { BurstCallback(); });

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
  std::vector<BurstReaderClient> radar_clients_;
  async::Loop loop_;

 private:
  // Assume that there are always this many connected clients with bursts started. A counter will be
  // compared to this value to ensure that the driver doesn't send a new burst until all clients
  // have unlocked their VMOs.
  static constexpr uint32_t kClientsWithBurstsStarted = 2;
  // Keep running until all clients receive this many bursts, then quit the loop.
  static constexpr uint32_t kReceiveBurstCount = 10;

  void BurstCallback() {
    static uint32_t client_lockstep_counter = 0;
    static uint32_t burst_count = 0;
    // Increment the counter until all clients have received this burst. At that point we can either
    // send another burst, or stop the loop.
    if (++client_lockstep_counter == kClientsWithBurstsStarted) {
      client_lockstep_counter = 0;
      if (++burst_count == kReceiveBurstCount) {
        burst_count = 0;
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

class RadarReaderProxyInjectionTest : public RadarReaderProxyTest {
 public:
  RadarReaderProxyInjectionTest() : loop_(&kAsyncLoopConfigNeverAttachToThread) {}

  void SetUp() override {
    fake_driver_.set_burst_size(16);

    RadarReaderProxyTest::SetUp();

    AddRadarDevice();

    {
      zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
      ASSERT_TRUE(endpoints.is_ok());
      reader_client_.emplace(std::move(endpoints->client), loop_.dispatcher(),
                             fit::bind_member<&RadarReaderProxyInjectionTest::OnBurst>(this));
      EXPECT_TRUE(dut_client_->Connect(std::move(endpoints->server)).is_ok());
    }

    {
      std::vector<uint32_t> vmo_ids;
      std::vector<zx::vmo> vmos;
      for (uint32_t i = 0; i < 15; i++) {
        EXPECT_OK(zx::vmo::create(16, 0, &vmos.emplace_back()));
        EXPECT_OK(vmos.back().duplicate(ZX_RIGHT_SAME_RIGHTS, &registered_vmos_.emplace_back()));
        vmo_ids.emplace_back(i + 1);
      }

      reader_client_.value()
          ->RegisterVmos({std::move(vmo_ids), std::move(vmos)})
          .Then([](auto& result) { EXPECT_TRUE(result.is_ok()); });
    }

    {
      zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstInjector>();
      ASSERT_TRUE(endpoints.is_ok());
      injector_client_.emplace(std::move(endpoints->client), loop_.dispatcher(),
                               [&](uint32_t bursts_id) {
                                 received_vmo_ids_.push_back(bursts_id);
                                 if (on_bursts_delivered_) {
                                   on_bursts_delivered_();
                                 }
                               });
      BindInjector(std::move(endpoints->server));
    }
  }

 protected:
  struct BurstInjectorClient : fidl::AsyncEventHandler<fuchsia_hardware_radar::RadarBurstInjector> {
   public:
    BurstInjectorClient(fidl::ClientEnd<fuchsia_hardware_radar::RadarBurstInjector> client_end,
                        async_dispatcher_t* dispatcher, std::function<void(uint32_t)> callback)
        : client(std::move(client_end), dispatcher, this),
          bursts_delivered_callback(std::move(callback)) {}

    void OnBurstsDelivered(
        fidl::Event<fuchsia_hardware_radar::RadarBurstInjector::OnBurstsDelivered>& event)
        override {
      bursts_delivered_callback(event.bursts_id());
    }

    fidl::Client<fuchsia_hardware_radar::RadarBurstInjector>& operator->() { return client; }

    fidl::Client<fuchsia_hardware_radar::RadarBurstInjector> client;
    fuchsia_hardware_radar::StatusCode error_status = fuchsia_hardware_radar::StatusCode::kSuccess;

   private:
    const std::function<void(uint32_t)> bursts_delivered_callback;
  };

  fidl::Request<fuchsia_hardware_radar::RadarBurstInjector::EnqueueBursts> CreateInjectionVmo(
      uint8_t count, uint8_t offset = 0) {
    zx::vmo vmo;
    EXPECT_OK(zx::vmo::create(count * 16, 0, &vmo));

    for (uint8_t i = 0; i < count; i++) {
      uint8_t byte = i + offset;
      EXPECT_OK(vmo.write(&byte, i * 16, sizeof(byte)));
    }

    return {{std::move(vmo), count}};
  }

  async::Loop loop_;
  std::optional<BurstReaderClient> reader_client_;
  std::optional<BurstInjectorClient> injector_client_;
  std::vector<uint8_t> received_burst_headers_;
  std::vector<zx::vmo> registered_vmos_;
  std::vector<uint32_t> received_vmo_ids_;
  fit::function<void(size_t)> on_burst_;
  fit::function<void(void)> on_bursts_delivered_;

 private:
  void OnBurst(uint32_t vmo_id) {
    ASSERT_GE(vmo_id, 1);
    ASSERT_LE(vmo_id, 15);

    uint8_t byte = 0;
    EXPECT_OK(registered_vmos_[vmo_id - 1].read(&byte, 0, sizeof(byte)));
    received_burst_headers_.push_back(byte);

    if (on_burst_) {
      on_burst_(received_burst_headers_.size());
    }
  }
};

TEST_F(RadarReaderProxyInjectionTest, InjectBursts) {
  on_burst_ = [&](size_t burst_count) {
    if (burst_count == 2) {
      // Start burst injection after two real bursts.
      injector_client_.value()->StartBurstInjection().Then(
          [&](auto& result) { EXPECT_TRUE(result.is_ok()); });

      // Immediately send a request to stop injection, which will get completed after all three
      // burst buffers (containing a total of eight bursts) have been sent.
      injector_client_.value()->StopBurstInjection().Then([&](auto& result) {
        EXPECT_TRUE(result.is_ok());
        // Send a burst from the driver to continue the test from the burst callback.
        fake_driver_.SendBurst();
      });
    } else if (burst_count == 12) {
      // Stop the loop after 12 total bursts.
      loop_.Quit();
    } else if (burst_count < 2 || burst_count > 10) {
      fake_driver_.SendBurst();
    }
  };

  EXPECT_TRUE(reader_client_.value()->StartBursts().is_ok());
  injector_client_.value()->EnqueueBursts(CreateInjectionVmo(3, 0)).Then([&](auto& result) {
    EXPECT_TRUE(result.is_ok());
  });
  injector_client_.value()->EnqueueBursts(CreateInjectionVmo(2, 3)).Then([&](auto& result) {
    EXPECT_TRUE(result.is_ok());
  });
  injector_client_.value()->EnqueueBursts(CreateInjectionVmo(3, 5)).Then([&](auto& result) {
    EXPECT_TRUE(result.is_ok());

    // Send a burst to the client to trigger the the test.
    reader_client_.value()->GetBurstProperties().Then(
        [&](auto& result) { fake_driver_.SendBurst(); });
  });

  loop_.Run();

  const std::vector<uint8_t> kExpectedBurstHeaders = {0xff, 0xff, 0x00, 0x01, 0x02, 0x03,
                                                      0x04, 0x05, 0x06, 0x07, 0xff, 0xff};
  ASSERT_EQ(received_burst_headers_.size(), kExpectedBurstHeaders.size());
  EXPECT_EQ(received_burst_headers_, kExpectedBurstHeaders);

  const std::vector<uint32_t> kExpectedVmoIds = {1, 2, 3};
  ASSERT_EQ(received_vmo_ids_.size(), kExpectedVmoIds.size());
  EXPECT_EQ(received_vmo_ids_, kExpectedVmoIds);
}

TEST_F(RadarReaderProxyInjectionTest, InjectionPausedWithNoEnqueuedVmos) {
  on_bursts_delivered_ = [&]() {
    if (received_vmo_ids_.size() == 1) {
      // The last burst from the original VMO has been received, now enqueue a new one. We should
      // still not miss any injected bursts.
      injector_client_.value()->EnqueueBursts(CreateInjectionVmo(2, 3)).Then([&](auto& result) {
        EXPECT_TRUE(result.is_ok());
      });
    } else if (received_vmo_ids_.size() == 2) {
      injector_client_.value()->StopBurstInjection().Then([&](auto& result) {
        EXPECT_TRUE(result.is_ok());
        // We know that the last bursts will have been sent on the reader client channel by the time
        // StopBurstInjection() replies. We can ensure that all bursts have been handled by sending
        // another request with the reader client then stopping the loop when the response is
        // received.
        reader_client_.value()->GetBurstProperties().Then([&](auto& result) { loop_.Quit(); });
      });
    }
  };

  // Start burst injection, then enqueue a burst VMO. Clients should not miss any injected bursts.
  injector_client_.value()->StartBurstInjection().Then([&](auto& result) {
    EXPECT_TRUE(result.is_ok());
    EXPECT_TRUE(reader_client_.value()->StartBursts().is_ok());
    reader_client_.value()->GetBurstProperties().Then([&](auto& result) {
      injector_client_.value()->EnqueueBursts(CreateInjectionVmo(3, 0)).Then([&](auto& result) {
        EXPECT_TRUE(result.is_ok());
      });
    });
  });

  loop_.Run();

  const std::vector<uint8_t> kExpectedBurstHeaders = {0, 1, 2, 3, 4};
  ASSERT_EQ(received_burst_headers_.size(), kExpectedBurstHeaders.size());
  EXPECT_EQ(received_burst_headers_, kExpectedBurstHeaders);

  // The buffer ID counter should be reset after the first one is returned to us.
  const std::vector<uint32_t> kExpectedVmoIds = {1, 1};
  ASSERT_EQ(received_vmo_ids_.size(), kExpectedVmoIds.size());
  EXPECT_EQ(received_vmo_ids_, kExpectedVmoIds);
}

TEST_F(RadarReaderProxyInjectionTest, BurstsNotDeliveredToStopppedClients) {
  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  EXPECT_TRUE(dut_client_->Connect(std::move(endpoints->server)).is_ok());

  BurstReaderClient started_client(std::move(endpoints->client), loop_.dispatcher(), [&](uint32_t) {
    if (started_client.burst_count == 3) {
      loop_.Quit();
    }
  });

  for (uint32_t i = 0; i < 3; i++) {
    std::vector<zx::vmo> vmos;
    EXPECT_OK(zx::vmo::create(16, 0, &vmos.emplace_back()));
    started_client->RegisterVmos({std::vector<uint32_t>{i}, std::move(vmos)})
        .Then([&](auto& result) { EXPECT_TRUE(result.is_ok()); });
  }

  EXPECT_TRUE(started_client->StartBursts().is_ok());
  started_client->GetBurstProperties().Then([&](auto& result) {
    injector_client_.value()->EnqueueBursts(CreateInjectionVmo(3, 0)).Then([&](auto& result) {
      EXPECT_TRUE(result.is_ok());
    });
    injector_client_.value()->StartBurstInjection().Then(
        [&](auto& result) { EXPECT_TRUE(result.is_ok()); });
  });

  loop_.Run();

  // The original (stopped) client should receive zero bursts, while the started client should
  // receive three.
  EXPECT_EQ(reader_client_->burst_count, 0);
  EXPECT_EQ(started_client.burst_count, 3);
}

TEST_F(RadarReaderProxyInjectionTest, BurstsReceivedDuringInjectionAreIgnored) {
  on_burst_ = [&](size_t) {
    if (received_burst_headers_.size() == 5 && received_vmo_ids_.size() == 1) {
      loop_.Quit();
    }
  };
  on_bursts_delivered_ = [&]() {
    if (received_burst_headers_.size() == 5 && received_vmo_ids_.size() == 1) {
      loop_.Quit();
    }
  };

  injector_client_.value()->EnqueueBursts(CreateInjectionVmo(5)).Then([&](auto& result) {
    EXPECT_TRUE(result.is_ok());
  });

  EXPECT_TRUE(reader_client_.value()->StartBursts().is_ok());
  reader_client_.value()->GetBurstProperties().Then([&](auto& result) {
    injector_client_.value()->StartBurstInjection().Then([&](auto& result) {
      EXPECT_TRUE(result.is_ok());
      // Send three bursts from the driver after injection has been enabled and the client has been
      // started. All three should be ignored in favor of the injected bursts.
      fake_driver_.SendBurst();
      fake_driver_.SendBurst();
      fake_driver_.SendBurst();
    });
  });

  loop_.Run();

  const std::vector<uint8_t> kExpectedBursts = {0, 1, 2, 3, 4};
  ASSERT_EQ(received_burst_headers_.size(), kExpectedBursts.size());
  EXPECT_EQ(received_burst_headers_, kExpectedBursts);

  const std::vector<uint32_t> kExpectedVmoIds = {1};
  ASSERT_EQ(received_vmo_ids_.size(), kExpectedVmoIds.size());
  EXPECT_EQ(received_vmo_ids_, kExpectedVmoIds);
}

TEST_F(RadarReaderProxyInjectionTest, CallsFailsWithBadState) {
  injector_client_.value()->StartBurstInjection().Then(
      [&](auto& result) { EXPECT_TRUE(result.is_ok()); });

  injector_client_.value()->StartBurstInjection().Then([&](auto& result) {
    ASSERT_FALSE(result.is_ok());
    ASSERT_TRUE(result.error_value().is_domain_error());
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_hardware_radar::StatusCode::kAlreadyBound);
  });

  injector_client_.value()->StopBurstInjection().Then(
      [&](auto& result) { EXPECT_TRUE(result.is_ok()); });

  injector_client_.value()->StopBurstInjection().Then([&](auto& result) {
    ASSERT_FALSE(result.is_ok());
    ASSERT_TRUE(result.error_value().is_domain_error());
    EXPECT_EQ(result.error_value().domain_error(),
              fuchsia_hardware_radar::StatusCode::kAlreadyBound);

    loop_.Quit();
  });

  loop_.Run();
}

TEST_F(RadarReaderProxyInjectionTest, OnlyOneInjectorCanConnect) {
  struct : public fidl::AsyncEventHandler<fuchsia_hardware_radar::RadarBurstInjector> {
    void on_fidl_error(fidl::UnbindInfo info) override {
      EXPECT_EQ(info.status(), ZX_ERR_ALREADY_BOUND);
      fidl_error_callback();
    }
    fit::callback<void(void)> fidl_error_callback;
  } client_disconnect_waiter;
  client_disconnect_waiter.fidl_error_callback = [&]() { loop_.Quit(); };

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstInjector>();
  ASSERT_TRUE(endpoints.is_ok());

  BindInjector(std::move(endpoints->server));

  fidl::Client<fuchsia_hardware_radar::RadarBurstInjector> client(
      std::move(endpoints->client), loop_.dispatcher(), &client_disconnect_waiter);

  loop_.Run();
}

TEST_F(RadarReaderProxyTest, ConnectInjectorBeforeRadarDevice) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstInjector>();
  ASSERT_TRUE(endpoints.is_ok());

  BindInjector(std::move(endpoints->server));

  fidl::Client<fuchsia_hardware_radar::RadarBurstInjector> client(std::move(endpoints->client),
                                                                  loop.dispatcher());

  // Issue two requests before adding the radar device. They should be fulfilled after the proxy
  // connects.
  client->StartBurstInjection().Then([&](auto& result) { EXPECT_TRUE(result.is_ok()); });
  client->StopBurstInjection().Then([&](auto& result) {
    EXPECT_TRUE(result.is_ok());
    loop.Quit();
  });

  AddRadarDevice();

  loop.Run();
}

}  // namespace
}  // namespace radar
