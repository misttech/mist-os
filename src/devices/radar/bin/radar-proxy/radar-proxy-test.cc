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

class FakeRadar : public fidl::Server<fuchsia_hardware_radar::RadarBurstReader> {
 public:
  void GetBurstSize(GetBurstSizeCompleter::Sync& completer) override { completer.Reply(12345); }
  void RegisterVmos(RegisterVmosRequest& request, RegisterVmosCompleter::Sync& completer) override {
  }
  void UnregisterVmos(UnregisterVmosRequest& request,
                      UnregisterVmosCompleter::Sync& completer) override {}
  void StartBursts(StartBurstsCompleter::Sync& completer) override {}
  void StopBursts(StopBurstsCompleter::Sync& completer) override {}
  void UnlockVmo(UnlockVmoRequest& request, UnlockVmoCompleter::Sync& completer) override {}
};

class FakeRadarDriver : public fidl::Server<fuchsia_hardware_radar::RadarBurstReaderProvider> {
 public:
  explicit FakeRadarDriver(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}

  void Connect(ConnectRequest& request, ConnectCompleter::Sync& completer) override {
    fidl::BindServer(dispatcher_, std::move(request.server()), &fake_radar_);
    completer.Reply(fit::ok());
  }

 private:
  FakeRadar fake_radar_;
  async_dispatcher_t* const dispatcher_;
};

class TestRadarDeviceConnector : public RadarDeviceConnector {
 public:
  explicit TestRadarDeviceConnector(async_dispatcher_t* dispatcher)
      : fake_driver_(dispatcher), dispatcher_(dispatcher) {}

  void ConnectToRadarDevice(int dir_fd, const std::filesystem::path& path,
                            ConnectDeviceCallback connect_device) override {
    ConnectToFirstRadarDevice(std::move(connect_device));
  }

  void ConnectToFirstRadarDevice(ConnectDeviceCallback connect_device) override {
    if (connect_fail_) {
      return;
    }

    zx::result endpoints =
        fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReaderProvider>();
    ASSERT_TRUE(endpoints.is_ok());

    driver_binding_ = fidl::BindServer(dispatcher_, std::move(endpoints->server), &fake_driver_);
    connect_device(std::move(endpoints->client));
  }

 private:
  FakeRadarDriver fake_driver_;

 public:
  // Visible for testing.
  bool connect_fail_ = false;
  std::optional<fidl::ServerBindingRef<fuchsia_hardware_radar::RadarBurstReaderProvider>>
      driver_binding_;
  async_dispatcher_t* const dispatcher_;
};

class RadarProxyTest : public zxtest::Test {
 public:
  RadarProxyTest()
      : loop_(&kAsyncLoopConfigAttachToCurrentThread),
        connector_(loop_.dispatcher()),
        proxy_(loop_.dispatcher(), &connector_) {}

  void SetUp() override {
    ASSERT_OK(loop_.StartThread());
    EXPECT_TRUE(fdf::RunOnDispatcherSync(loop_.dispatcher(), [&]() {
                  zx::result endpoints =
                      fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReaderProvider>();
                  ASSERT_TRUE(endpoints.is_ok());
                  fidl::BindServer(loop_.dispatcher(), std::move(endpoints->server), &proxy_);
                  service_client_.Bind(std::move(endpoints->client));
                }).is_ok());
  }

  void TearDown() override {
    // Close the binding and make sure the proxy doesn't try to reconnect after the loop has been
    // stopped.
    EXPECT_TRUE(fdf::RunOnDispatcherSync(loop_.dispatcher(), [&]() {
                  connector_.connect_fail_ = true;
                  connector_.driver_binding_->Close(ZX_ERR_PEER_CLOSED);
                }).is_ok());
    loop_.Shutdown();
  }

 protected:
  // Visible for testing.
  async::Loop loop_;
  TestRadarDeviceConnector connector_;
  RadarProviderProxy proxy_;
  fidl::SyncClient<fuchsia_hardware_radar::RadarBurstReaderProvider> service_client_;
};

TEST_F(RadarProxyTest, Connect) {
  EXPECT_TRUE(fdf::RunOnDispatcherSync(loop_.dispatcher(), [&]() {
                proxy_.DeviceAdded(0, "000");
              }).is_ok());

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  fidl::SyncClient<fuchsia_hardware_radar::RadarBurstReader> radar_client(
      std::move(endpoints->client));

  {
    auto result = service_client_->Connect(std::move(endpoints->server));
    EXPECT_TRUE(result.is_ok());
  }

  {
    auto result = radar_client->GetBurstSize();
    EXPECT_TRUE(result.is_ok());
    EXPECT_EQ(result->burst_size(), 12345);
  }
}

TEST_F(RadarProxyTest, Reconnect) {
  EXPECT_TRUE(fdf::RunOnDispatcherSync(loop_.dispatcher(), [&]() {
                proxy_.DeviceAdded(0, "000");
              }).is_ok());

  zx::result endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  fidl::SyncClient<fuchsia_hardware_radar::RadarBurstReader> radar_client(
      std::move(endpoints->client));

  {
    auto result = service_client_->Connect(std::move(endpoints->server));
    EXPECT_TRUE(result.is_ok());
  }

  {
    auto result = radar_client->GetBurstSize();
    EXPECT_TRUE(result.is_ok());
    EXPECT_EQ(result->burst_size(), 12345);
  }

  EXPECT_TRUE(fdf::RunOnDispatcherSync(loop_.dispatcher(), [&]() {
                // Close the connection between the proxy and the radar driver. Calls to Connect()
                // should immediately start failing.
                connector_.connect_fail_ = true;
                connector_.driver_binding_->Close(ZX_ERR_PEER_CLOSED);
              }).is_ok());

  // We should still be able to communicate with the radar proxy, but the call to Connect() should
  // return an error.
  endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  fidl::SyncClient<fuchsia_hardware_radar::RadarBurstReader> new_radar_client(
      std::move(endpoints->client));

  {
    auto result = service_client_->Connect(std::move(endpoints->server));
    EXPECT_FALSE(result.is_ok());
  }

  // The connection with the radar driver itself should remain open.
  {
    auto result = radar_client->GetBurstSize();
    EXPECT_TRUE(result.is_ok());
    EXPECT_EQ(result->burst_size(), 12345);
  }

  EXPECT_TRUE(fdf::RunOnDispatcherSync(loop_.dispatcher(), [&]() {
                // Signal that a new device was added to /dev/class/radar.
                connector_.connect_fail_ = false;
                proxy_.DeviceAdded(0, "000");
              }).is_ok());

  // The proxy should now be able to connect to the new radar driver.
  endpoints = fidl::CreateEndpoints<fuchsia_hardware_radar::RadarBurstReader>();
  ASSERT_TRUE(endpoints.is_ok());

  new_radar_client = fidl::SyncClient(std::move(endpoints->client));

  {
    auto result = service_client_->Connect(std::move(endpoints->server));
    EXPECT_FALSE(result.is_ok());
  }

  {
    auto result = radar_client->GetBurstSize();
    EXPECT_TRUE(result.is_ok());
    EXPECT_EQ(result->burst_size(), 12345);
  }
}

}  // namespace radar
