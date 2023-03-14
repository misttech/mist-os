// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "radar-proxy.h"

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/fidl/cpp/binding.h>

#include <zxtest/zxtest.h>

#include "sdk/lib/driver/runtime/testing/runtime/dispatcher.h"

namespace radar {

class FakeRadar : public fuchsia::hardware::radar::RadarBurstReader {
 public:
  void GetBurstSize(GetBurstSizeCallback callback) override { callback(12345); }
  void RegisterVmos(std::vector<uint32_t> vmo_ids, std::vector<::zx::vmo> vmos,
                    RegisterVmosCallback callback) override {}
  void UnregisterVmos(std::vector<uint32_t> vmo_ids, UnregisterVmosCallback callback) override {}
  void StartBursts() override {}
  void StopBursts(StopBurstsCallback callback) override {}
  void UnlockVmo(uint32_t vmo_id) override {}
};

class FakeRadarDriver : public fuchsia::hardware::radar::RadarBurstReaderProvider {
 public:
  FakeRadarDriver() : radar_binding_(&fake_radar_) {}

  void Connect(fidl::InterfaceRequest<fuchsia::hardware::radar::RadarBurstReader> server,
               ConnectCallback callback) override {
    fuchsia::hardware::radar::RadarBurstReaderProvider_Connect_Result result;
    fuchsia::hardware::radar::RadarBurstReaderProvider_Connect_Response response;
    if (radar_binding_.Bind(std::move(server)) == ZX_OK) {
      result.set_response(response);
    } else {
      result.set_err(fuchsia::hardware::radar::StatusCode::BIND_ERROR);
    }

    callback(std::move(result));
  }

 private:
  FakeRadar fake_radar_;
  fidl::Binding<fuchsia::hardware::radar::RadarBurstReader> radar_binding_;
};

class TestRadarDeviceConnector : public RadarDeviceConnector {
 public:
  TestRadarDeviceConnector() : driver_binding_(&fake_driver_) {}

  fuchsia::hardware::radar::RadarBurstReaderProviderPtr ConnectToRadarDevice(
      int dir_fd, const std::string& filename) override {
    return ConnectToFirstRadarDevice();
  }

  fuchsia::hardware::radar::RadarBurstReaderProviderPtr ConnectToFirstRadarDevice() override {
    if (connect_fail_) {
      return {};
    }
    fuchsia::hardware::radar::RadarBurstReaderProviderPtr radar_client;
    radar_client.Bind(driver_binding_.NewBinding().TakeChannel());
    return radar_client;
  }

 private:
  FakeRadarDriver fake_driver_;

 public:
  // Visible for testing.
  bool connect_fail_ = false;
  fidl::Binding<fuchsia::hardware::radar::RadarBurstReaderProvider> driver_binding_;
};

class RadarProxyTest : public zxtest::Test {
 public:
  RadarProxyTest()
      : proxy_(&connector_),
        proxy_binding_(&proxy_),
        loop_(&kAsyncLoopConfigAttachToCurrentThread) {}

  void SetUp() override {
    ASSERT_OK(loop_.StartThread());
    EXPECT_TRUE(fdf::RunOnDispatcherSync(loop_.dispatcher(), [&]() {
                  ASSERT_OK(proxy_binding_.Bind(service_client_.NewRequest()));
                }).is_ok());
  }

 protected:
  // Visible for testing.
  TestRadarDeviceConnector connector_;
  RadarProxy proxy_;
  fuchsia::hardware::radar::RadarBurstReaderProviderSyncPtr service_client_;

 private:
  fidl::Binding<fuchsia::hardware::radar::RadarBurstReaderProvider> proxy_binding_;

 protected:
  async::Loop loop_;
};

TEST_F(RadarProxyTest, Connect) {
  EXPECT_TRUE(fdf::RunOnDispatcherSync(loop_.dispatcher(), [&]() {
                proxy_.DeviceAdded(0, "000");
              }).is_ok());

  fuchsia::hardware::radar::RadarBurstReaderSyncPtr radar_client;
  fuchsia::hardware::radar::RadarBurstReaderProvider_Connect_Result connect_result;
  EXPECT_OK(service_client_->Connect(radar_client.NewRequest(), &connect_result));
  EXPECT_TRUE(connect_result.is_response());

  uint32_t burst_size = 0;
  EXPECT_OK(radar_client->GetBurstSize(&burst_size));
  EXPECT_EQ(burst_size, 12345);
}

TEST_F(RadarProxyTest, Reconnect) {
  EXPECT_TRUE(fdf::RunOnDispatcherSync(loop_.dispatcher(), [&]() {
                proxy_.DeviceAdded(0, "000");
              }).is_ok());

  fuchsia::hardware::radar::RadarBurstReaderSyncPtr radar_client;
  fuchsia::hardware::radar::RadarBurstReaderProvider_Connect_Result connect_result;
  EXPECT_OK(service_client_->Connect(radar_client.NewRequest(), &connect_result));
  EXPECT_TRUE(connect_result.is_response());

  uint32_t burst_size = 0;
  EXPECT_OK(radar_client->GetBurstSize(&burst_size));
  EXPECT_EQ(burst_size, 12345);

  EXPECT_TRUE(fdf::RunOnDispatcherSync(loop_.dispatcher(), [&]() {
                // Close the connection between the proxy and the radar driver. Calls to Connect()
                // should immediately start failing.
                connector_.connect_fail_ = true;
                EXPECT_OK(connector_.driver_binding_.Close(ZX_ERR_PEER_CLOSED));
              }).is_ok());

  // We should still be able to communicate with the radar proxy, but the call to Connect() should
  // return an error.
  fuchsia::hardware::radar::RadarBurstReaderSyncPtr new_radar_client;
  EXPECT_OK(service_client_->Connect(new_radar_client.NewRequest(), &connect_result));
  EXPECT_TRUE(connect_result.is_err());

  // The connection with the radar driver itself should remain open.
  EXPECT_OK(radar_client->GetBurstSize(&burst_size));
  EXPECT_EQ(burst_size, 12345);

  EXPECT_TRUE(fdf::RunOnDispatcherSync(loop_.dispatcher(), [&]() {
                // Signal that a new device was added to /dev/class/radar.
                connector_.connect_fail_ = false;
                proxy_.DeviceAdded(0, "000");
              }).is_ok());

  // The proxy should now be able to connect to the new radar driver.
  EXPECT_OK(service_client_->Connect(new_radar_client.NewRequest(), &connect_result));

  EXPECT_OK(new_radar_client->GetBurstSize(&burst_size));
  EXPECT_EQ(burst_size, 12345);
}

}  // namespace radar
