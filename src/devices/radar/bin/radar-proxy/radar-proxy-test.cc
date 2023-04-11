// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>

#include <optional>

#include <zxtest/zxtest.h>

#include "radar-provider-proxy.h"
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

 private:
  void Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) override {
    fidl::BindServer(dispatcher_, std::move(request.server()), this);
    completer.Reply(fit::ok());
  }
  void GetBurstSize(GetBurstSizeCompleter::Sync& completer) override { completer.Reply(12345); }
  void RegisterVmos(RegisterVmosRequest& request, RegisterVmosCompleter::Sync& completer) override {
  }
  void UnregisterVmos(UnregisterVmosRequest& request,
                      UnregisterVmosCompleter::Sync& completer) override {}
  void StartBursts(StartBurstsCompleter::Sync& completer) override {}
  void StopBursts(StopBurstsCompleter::Sync& completer) override {}
  void UnlockVmo(UnlockVmoRequest& request, UnlockVmoCompleter::Sync& completer) override {}

  async_dispatcher_t* const dispatcher_;
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_radar::RadarBurstReaderProvider>>
      provider_binding_;
};

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
  // Closes the RadarBurstProvider connection between radar-proxy and the radar driver, and waits
  // for radar-proxy to process the epitaph. After this call, requests to radar-proxy should either
  // succeed or fail depending on the value of provider_connect_fail_.
  void UnbindProviderAndWaitForConnectionAttempt() {
    sync_completion_reset(&device_connected_);
    UnbindRadarDevice();
    sync_completion_wait(&device_connected_, ZX_TIME_INFINITE);
  }

  void AddRadarDevice() {
    EXPECT_TRUE(fdf::RunOnDispatcherSync(proxy_loop_.dispatcher(), [&]() {
                  provider_connect_fail_ = false;
                  dut_.DeviceAdded(kInvalidDir, "000");
                }).is_ok());
  }

  // Visible for testing.
  fidl::SyncClient<fuchsia_hardware_radar::RadarBurstReaderProvider> dut_client_;

 private:
  static constexpr fidl::UnownedClientEnd<fuchsia_io::Directory> kInvalidDir{FIDL_HANDLE_INVALID};

  void UnbindRadarDevice() {
    EXPECT_TRUE(fdf::RunOnDispatcherSync(proxy_loop_.dispatcher(), [&]() {
                  provider_connect_fail_ = true;
                }).is_ok());
    fake_driver_.UnbindProvider();
  }

  FakeRadarDriver fake_driver_;
  RadarProviderProxy dut_;

  bool provider_connect_fail_ = false;
  sync_completion_t device_connected_;
};

TEST_F(RadarProxyTest, Connect) {
  AddRadarDevice();

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  auto result = dut_client_->Connect(std::move(endpoints->server));
  EXPECT_TRUE(result.is_ok());

  fidl::SyncClient radar_client(std::move(endpoints->client));
  EXPECT_EQ(radar_client->GetBurstSize().value_or(0).burst_size(), 12345);
}

TEST_F(RadarProxyTest, Reconnect) {
  async::Loop loop(&kAsyncLoopConfigNeverAttachToThread);

  fidl::Client service_client(dut_client_.TakeClientEnd(), loop.dispatcher());

  AddRadarDevice();

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  service_client->Connect(std::move(endpoints->server)).Then([&](auto& result) {
    EXPECT_TRUE(result.is_ok());

    fidl::SyncClient radar_client(std::move(endpoints->client));
    EXPECT_EQ(radar_client->GetBurstSize().value_or(0).burst_size(), 12345);

    // Close the connection between the proxy and the radar driver. Calls to Connect() should
    // then be put in the request queue instead of immediately completed.
    UnbindProviderAndWaitForConnectionAttempt();

    // The connection with the radar driver itself should remain open.
    EXPECT_EQ(radar_client->GetBurstSize().value_or(0).burst_size(), 12345);

    endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
    ASSERT_TRUE(endpoints.is_ok());

    // Issue a connect request to be completed when a new device shows up.
    service_client->Connect(std::move(endpoints->server)).Then([&](auto& result) {
      EXPECT_TRUE(result.is_ok());

      // The proxy connected to the new device and completed our request. We should now have a new
      // connection to the underlying driver.
      fidl::SyncClient new_radar_client(std::move(endpoints->client));
      EXPECT_EQ(new_radar_client->GetBurstSize().value_or(0).burst_size(), 12345);

      loop.Quit();
    });

    // Signal that a new device was added to /dev/class/radar. The proxy should connect to the new
    // device and complete our previous request.
    AddRadarDevice();
  });

  loop.Run();
}

TEST_F(RadarProxyTest, DelayedConnect) {
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
  radar_client->GetBurstSize().Then([&](auto& result) {
    burst_size = result.value_or(0).burst_size();
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

}  // namespace
}  // namespace radar
