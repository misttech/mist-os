// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_NETWORK_DEVICE_TEST_BANJO_H_
#define SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_NETWORK_DEVICE_TEST_BANJO_H_

#include <fuchsia/hardware/network/driver/cpp/banjo.h>
#include <lib/driver/compat/cpp/compat.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/testing/cpp/driver_test.h>

#include <ddktl/device.h>
#include <gtest/gtest.h>

#include "device/test_session.h"
#include "device/test_util_banjo.h"
#include "mac/test_util_banjo.h"
#include "network_device.h"

namespace network::testing {

// The environment represents what exists before the driver starts. This includes the parent driver
// which implements NetworkDeviceImpl. In the tests this is done by FakeNetworkDeviceImpl, therefore
// an object of that type must live in the environment to provide a discoverable service for the
// network device driver to connect to.
struct BanjoTestFixtureEnvironment : public fdf_testing::Environment {
  zx::result<> Serve(fdf::OutgoingDirectory& outgoing) override {
    compat::DeviceServer::BanjoConfig banjo_config{ZX_PROTOCOL_NETWORK_DEVICE_IMPL};
    banjo_config.callbacks[ZX_PROTOCOL_NETWORK_DEVICE_IMPL] = [this]() {
      return compat::DeviceServer::GenericProtocol{.ops = device_impl_.proto().ops,
                                                   .ctx = device_impl_.proto().ctx};
    };
    device_server_.Initialize(component::kDefaultInstance, std::nullopt, std::move(banjo_config));
    return zx::make_result(
        device_server_.Serve(fdf::Dispatcher::GetCurrent()->async_dispatcher(), &outgoing));
  }

  banjo::FakeNetworkDeviceImpl device_impl_;
  compat::DeviceServer device_server_;
};

struct BanjoTestFixtureConfig {
  using DriverType = network::NetworkDevice;
  using EnvironmentType = BanjoTestFixtureEnvironment;
};

class BanjoNetDeviceDriverTest : public ::testing::Test {
 public:
  fdf_testing::BackgroundDriverTest<BanjoTestFixtureConfig>& driver_test() { return driver_test_; }

 protected:
  // Use a nonzero port identifier to avoid default value traps.
  static constexpr uint8_t kPortId = 11;

  void SetUp() override {
    zx::result<> result = driver_test().StartDriver();
    ASSERT_EQ(ZX_OK, result.status_value());
  }

  void TearDown() override { ShutdownDriver(); }

  void ShutdownDriver() {
    if (shutdown_) {
      return;
    }
    EXPECT_OK(driver_test().StopDriver().status_value());
    shutdown_ = true;
  }

  zx_status_t CreateDevice(bool with_mac = false) {
    if (with_mac) {
      port_impl_.SetMac(mac_impl_.proto());
    }
    port_impl_.SetStatus(
        {.flags = static_cast<uint32_t>(netdev::wire::StatusFlags::kOnline), .mtu = 2048});
    return driver_test().RunInEnvironmentTypeContext<zx_status_t>(
        [&](BanjoTestFixtureEnvironment& env) {
          return port_impl_.AddPort(kPortId, env.device_impl_.client());
        });
  }

  zx::result<fidl::WireSyncClient<netdev::Device>> ConnectNetDevice() {
    auto [inst_client_end, inst_server_end] = fidl::Endpoints<netdev::DeviceInstance>::Create();
    driver_test().RunInDriverContext(
        [this, server_end = std::move(inst_server_end)](network::NetworkDevice& driver) mutable {
          fidl::BindServer(fidl_dispatcher_, std::move(server_end), &driver);
        });
    fidl::WireSyncClient instance_client(std::move(inst_client_end));

    auto [dev_client_end, dev_server_end] = fidl::Endpoints<netdev::Device>::Create();
    fidl::Status result = instance_client->GetDevice(std::move(dev_server_end));
    if (zx_status_t status = result.status(); status != ZX_OK) {
      return zx::error(status);
    }

    return zx::ok(fidl::WireSyncClient(std::move(dev_client_end)));
  }

  banjo::FakeNetworkPortImpl& port_impl() { return port_impl_; }

  zx::result<netdev::wire::PortId> GetSaltedPortId(uint8_t base_id) {
    // List all existing ports from the device until we find the right port id.
    zx::result connect_result = ConnectNetDevice();
    if (connect_result.is_error()) {
      return connect_result.take_error();
    }
    fidl::WireSyncClient<netdev::Device>& netdevice = connect_result.value();
    auto [client_end, server_end] = fidl::Endpoints<netdev::PortWatcher>::Create();
    if (zx_status_t status = netdevice->GetPortWatcher(std::move(server_end)).status();
        status != ZX_OK) {
      return zx::error(status);
    }
    fidl::WireSyncClient watcher{std::move(client_end)};
    for (;;) {
      fidl::WireResult result = watcher->Watch();
      if (!result.ok()) {
        return zx::error(result.status());
      }
      const netdev::wire::DevicePortEvent& event = result.value().event;

      netdev::wire::PortId id;
      switch (event.Which()) {
        case netdev::wire::DevicePortEvent::Tag::kAdded:
          id = event.added();
          break;
        case netdev::wire::DevicePortEvent::Tag::kExisting:
          id = event.existing();
          break;
        case netdev::wire::DevicePortEvent::Tag::kIdle:
        case netdev::wire::DevicePortEvent::Tag::kRemoved:
          ADD_FAILURE() << "Unexpected port watcher event " << static_cast<uint32_t>(event.Which());
          return zx::error(ZX_ERR_INTERNAL);
      }
      if (id.base == base_id) {
        return zx::ok(id);
      }
    }
  }

  zx_status_t AttachSessionPort(TestSession& session, banjo::FakeNetworkPortImpl& impl) {
    std::vector<netdev::wire::FrameType> rx_types;
    for (uint8_t frame_type :
         cpp20::span(impl.port_info().rx_types_list, impl.port_info().rx_types_count)) {
      rx_types.push_back(static_cast<netdev::wire::FrameType>(frame_type));
    }
    zx::result port_id = GetSaltedPortId(impl.id());
    if (port_id.is_error()) {
      return port_id.status_value();
    }
    return session.AttachPort(port_id.value(), std::move(rx_types));
  }

  zx_status_t WaitForEvent(zx_signals_t event, zx::duration timeout = zx::duration::infinite()) {
    // Don't wait inside the environment context as that might be running on the dispatcher that
    // triggers the event. Waiting on it would then deadlock.
    zx::unowned_event events = driver_test().RunInEnvironmentTypeContext<zx::unowned_event>(
        [&](BanjoTestFixtureEnvironment& env) { return env.device_impl_.events().borrow(); });
    return events->wait_one(event, zx::deadline_after(timeout), nullptr);
  }

 private:
  fdf_testing::BackgroundDriverTest<BanjoTestFixtureConfig> driver_test_;
  bool shutdown_ = false;
  async_dispatcher_t* fidl_dispatcher_ =
      driver_test().runtime().StartBackgroundDispatcher()->async_dispatcher();

  banjo::FakeMacDeviceImpl mac_impl_;
  banjo::FakeNetworkPortImpl port_impl_;
};

}  // namespace network::testing

#endif  // SRC_CONNECTIVITY_NETWORK_DRIVERS_NETWORK_DEVICE_NETWORK_DEVICE_TEST_BANJO_H_
