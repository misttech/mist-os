// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.wlan.phyimpl/cpp/driver/wire.h>
#include <fidl/fuchsia.wlan.phyimpl/cpp/wire_types.h>
#include <fuchsia/wlan/common/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/async/default.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/driver/testing/cpp/driver_test.h>
#include <lib/fdf/testing.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fidl/cpp/decoder.h>
#include <lib/fidl/cpp/message.h>
#include <lib/sync/cpp/completion.h>
#include <lib/sys/cpp/testing/component_context_provider.h>
#include <netinet/if_ether.h>
#include <zircon/errors.h>

#include <gtest/gtest.h>

#include "src/connectivity/wlan/drivers/wlanphy/device.h"
#include "src/devices/bin/driver_runtime/dispatcher.h"

namespace wlanphy {
namespace {

// This test class provides the fake upper layer(wlandevicemonitor) and lower layer(wlanphyimpl
// device) for running wlanphy device:
//    |
//    |                              +--------------------+
//    |                             /|  wlandevicemonitor |
//    |                            / +--------------------+
//    |                           /            |
//    |                          /             | <---- [Normal FIDL with protocol:
//    |                         /              |                fuchsia_wlan_device::Phy]
//    |             Both faked           +-----+-----+
//    |            in this test          |  wlanphy  |   <---- Test target
//    |        class(WlanDeviceTest)     |  device   |
//    |                         \        +-----+-----+
//    |                          \             |
//    |                           \            | <---- [Driver transport FIDL with protocol:
//    |                            \           |          fuchsia_wlan_phyimpl::WlanPhyImpl]
//    |                             \  +---------------+
//    |                              \ | wlanphyimpl   |
//    |                                |    device     |
//    |                                +---------------+
//    |
class FakeWlanPhyImpl : public fdf::WireServer<fuchsia_wlan_phyimpl::WlanPhyImpl> {
 public:
  FakeWlanPhyImpl() {
    // Initialize struct to avoid random values.
    memset(static_cast<void*>(&create_iface_req_), 0, sizeof(create_iface_req_));
  }

  ~FakeWlanPhyImpl() {}

  void ServiceConnectHandler(fdf::ServerEnd<fuchsia_wlan_phyimpl::WlanPhyImpl> server_end) {
    fdf::BindServer(fdf_dispatcher_get_current_dispatcher(), std::move(server_end), this);
  }
  // Server end handler functions for fuchsia_wlan_phyimpl::WlanPhyImpl.
  void GetSupportedMacRoles(fdf::Arena& arena,
                            GetSupportedMacRolesCompleter::Sync& completer) override {
    std::vector<fuchsia_wlan_common::wire::WlanMacRole> supported_mac_roles_vec;
    supported_mac_roles_vec.push_back(kFakeMacRole);
    auto supported_mac_roles =
        fidl::VectorView<fuchsia_wlan_common::wire::WlanMacRole>::FromExternal(
            supported_mac_roles_vec);
    fidl::Arena fidl_arena;
    auto builder =
        fuchsia_wlan_phyimpl::wire::WlanPhyImplGetSupportedMacRolesResponse::Builder(fidl_arena);
    builder.supported_mac_roles(supported_mac_roles);
    completer.buffer(arena).ReplySuccess(builder.Build());
    test_completion_.Signal();
  }
  void CreateIface(CreateIfaceRequestView request, fdf::Arena& arena,
                   CreateIfaceCompleter::Sync& completer) override {
    has_init_sta_addr_ = false;
    if (request->has_init_sta_addr()) {
      create_iface_req_.init_sta_addr = request->init_sta_addr();
      has_init_sta_addr_ = true;
    }
    if (request->has_role()) {
      create_iface_req_.role = request->role();
    }

    fidl::Arena fidl_arena;
    auto builder = fuchsia_wlan_phyimpl::wire::WlanPhyImplCreateIfaceResponse::Builder(fidl_arena);
    builder.iface_id(kFakeIfaceId);
    completer.buffer(arena).ReplySuccess(builder.Build());
    test_completion_.Signal();
  }
  void DestroyIface(DestroyIfaceRequestView request, fdf::Arena& arena,
                    DestroyIfaceCompleter::Sync& completer) override {
    destroy_iface_id_ = request->iface_id();
    completer.buffer(arena).ReplySuccess();
    test_completion_.Signal();
  }
  void SetCountry(SetCountryRequestView request, fdf::Arena& arena,
                  SetCountryCompleter::Sync& completer) override {
    country_ = *request;
    completer.buffer(arena).ReplySuccess();
    test_completion_.Signal();
  }
  void ClearCountry(fdf::Arena& arena, ClearCountryCompleter::Sync& completer) override {
    completer.buffer(arena).ReplySuccess();
    test_completion_.Signal();
  }
  void GetCountry(fdf::Arena& arena, GetCountryCompleter::Sync& completer) override {
    auto country = fuchsia_wlan_phyimpl::wire::WlanPhyCountry::WithAlpha2(kAlpha2);
    completer.buffer(arena).ReplySuccess(country);
    test_completion_.Signal();
  }
  void SetPowerSaveMode(SetPowerSaveModeRequestView request, fdf::Arena& arena,
                        SetPowerSaveModeCompleter::Sync& completer) override {
    ps_mode_ = request->ps_mode();
    completer.buffer(arena).ReplySuccess();
    test_completion_.Signal();
  }
  void GetPowerSaveMode(fdf::Arena& arena, GetPowerSaveModeCompleter::Sync& completer) override {
    fidl::Arena fidl_arena;
    auto builder =
        fuchsia_wlan_phyimpl::wire::WlanPhyImplGetPowerSaveModeResponse::Builder(fidl_arena);
    builder.ps_mode(kFakePsMode);

    completer.buffer(arena).ReplySuccess(builder.Build());
    test_completion_.Signal();
  }
  void handle_unknown_method(
      fidl::UnknownMethodMetadata<fuchsia_wlan_phyimpl::WlanPhyImpl> metadata,
      fidl::UnknownMethodCompleter::Sync& completer) override {}

  void WaitForCompletion() { test_completion_.Wait(); }

  bool HasInitStaAddr() { return has_init_sta_addr_; }
  fuchsia_wlan_device::wire::CreateIfaceRequest& GetIfaceReq() { return create_iface_req_; }
  uint16_t GetDestroyIfaceId() { return destroy_iface_id_; }
  fuchsia_wlan_phyimpl::wire::WlanPhyCountry& GetCountryReq() { return country_; }
  fuchsia_wlan_common::wire::PowerSaveType GetPSType() { return ps_mode_; }
  // Record the create iface request data when fake phyimpl device gets it.
  fuchsia_wlan_device::wire::CreateIfaceRequest create_iface_req_;
  bool has_init_sta_addr_;

  // Record the destroy iface request data when fake phyimpl device gets it.
  uint16_t destroy_iface_id_;

  // Record the country data when fake phyimpl device gets it.
  fuchsia_wlan_phyimpl::wire::WlanPhyCountry country_;

  // Record the power save mode data when fake phyimpl device gets it.
  fuchsia_wlan_common::wire::PowerSaveType ps_mode_;

  static constexpr fuchsia_wlan_common::wire::WlanMacRole kFakeMacRole =
      fuchsia_wlan_common::wire::WlanMacRole::kAp;
  static constexpr uint16_t kFakeIfaceId = 1;
  static constexpr fidl::Array<uint8_t, fuchsia_wlan_phyimpl::wire::kWlanphyAlpha2Len> kAlpha2{'W',
                                                                                               'W'};
  static constexpr fuchsia_wlan_common::wire::PowerSaveType kFakePsMode =
      fuchsia_wlan_common::wire::PowerSaveType::kPsModePerformance;
  static constexpr ::fidl::Array<uint8_t, 6> kValidStaAddr = {1, 2, 3, 4, 5, 6};
  static constexpr ::fidl::Array<uint8_t, 6> kInvalidStaAddr = {0, 0, 0, 0, 0, 0};

  // The completion to synchronize the state in tests, because there are async FIDL calls.
  libsync::Completion test_completion_;

 protected:
  void* dummy_ctx_;
};

class TestEnvironment : fdf_testing::Environment {
 public:
  zx::result<> Serve(fdf::OutgoingDirectory& to_driver_vfs) override {
    auto wlanphyimpl = [this](fdf::ServerEnd<fuchsia_wlan_phyimpl::WlanPhyImpl> server_end) {
      fake_phyimpl_parent_.ServiceConnectHandler(std::move(server_end));
    };

    // Add the service contains WlanPhyImpl protocol to outgoing directory.
    fuchsia_wlan_phyimpl::Service::InstanceHandler wlanphyimpl_service_handler(
        {.wlan_phy_impl = wlanphyimpl});
    auto result = to_driver_vfs.AddService<fuchsia_wlan_phyimpl::Service>(
        std::move(wlanphyimpl_service_handler));
    EXPECT_TRUE(result.is_ok());

    return zx::ok();
  }

  FakeWlanPhyImpl fake_phyimpl_parent_;
};

class FixtureConfig final {
 public:
  using DriverType = wlanphy::Device;
  using EnvironmentType = TestEnvironment;
};

class WlanphyDeviceTest : public ::testing::Test {
 public:
  WlanphyDeviceTest() = default;
  void SetUp() override {
    zx::result<> result = driver_test().StartDriver();
    ASSERT_EQ(ZX_OK, result.status_value());

    auto connect_result =
        driver_test().ConnectThroughDevfs<fuchsia_wlan_device::Connector>("wlanphy");
    EXPECT_EQ(ZX_OK, connect_result.status_value());
    // Bind to the client end
    fidl::ClientEnd<fuchsia_wlan_device::Connector> client_end(std::move(connect_result.value()));
    phy_connector_.Bind(std::move(client_end));
    ASSERT_TRUE(phy_connector_.is_valid());
    // Create an endpoint
    auto endpoints_phy = fidl::Endpoints<fuchsia_wlan_device::Phy>::Create();
    // Send the server end to the driver
    auto conn_result = phy_connector_->Connect(std::move(endpoints_phy.server));
    ASSERT_TRUE(conn_result.ok());
    // Bind to the client end.
    client_phy_ = fidl::WireSyncClient<fuchsia_wlan_device::Phy>(std::move(endpoints_phy.client));
    ASSERT_TRUE(client_phy_.is_valid());
  }

  void TearDown() override {
    // Only PrepareStop() will be called in StopDriver(), Stop() won't be called.
    zx::result prepare_stop_result = driver_test().StopDriver();
    EXPECT_EQ(ZX_OK, prepare_stop_result.status_value());
  }
  void WaitForCommandCompletion() {
    driver_test().RunInEnvironmentTypeContext(
        [](TestEnvironment& env) { return env.fake_phyimpl_parent_.WaitForCompletion(); });
  }
  fdf_testing::BackgroundDriverTest<FixtureConfig>& driver_test() { return driver_test_; }
  // The FIDL client to communicate with wlanphy device.
  fidl::WireSyncClient<fuchsia_wlan_device::Phy> client_phy_;
  fidl::WireSyncClient<fuchsia_wlan_device::Connector> phy_connector_;
  fdf_testing::BackgroundDriverTest<FixtureConfig> driver_test_;
};

TEST_F(WlanphyDeviceTest, CreateIfaceTestNullAddr) {
  auto dummy_ends = fidl::CreateEndpoints<fuchsia_wlan_device::Phy>();
  auto dummy_channel = dummy_ends->server.TakeChannel();
  // All-zero MAC address in the request will should result in a false on has_init_sta_addr in
  // next level's FIDL request.
  fuchsia_wlan_device::wire::CreateIfaceRequest req = {
      .role = fuchsia_wlan_common::wire::WlanMacRole::kClient,
      .mlme_channel = std::move(dummy_channel),
      .init_sta_addr = FakeWlanPhyImpl::kInvalidStaAddr,
  };
  auto result = client_phy_->CreateIface(std::move(req));
  ASSERT_TRUE(result.ok());

  WaitForCommandCompletion();
  EXPECT_FALSE(driver_test().RunInEnvironmentTypeContext<bool>(
      [](TestEnvironment& env) { return env.fake_phyimpl_parent_.HasInitStaAddr(); }));
}

TEST_F(WlanphyDeviceTest, CreateIfaceTestValidAddr) {
  auto dummy_ends = fidl::CreateEndpoints<fuchsia_wlan_device::Phy>();
  auto dummy_channel = dummy_ends->server.TakeChannel();

  fuchsia_wlan_device::wire::CreateIfaceRequest req = {
      .role = fuchsia_wlan_common::wire::WlanMacRole::kClient,
      .mlme_channel = std::move(dummy_channel),
      .init_sta_addr = FakeWlanPhyImpl::kValidStaAddr,
  };

  auto result = client_phy_->CreateIface(std::move(req));
  ASSERT_TRUE(result.ok());
  WaitForCommandCompletion();
  EXPECT_TRUE(driver_test().RunInEnvironmentTypeContext<bool>(
      [](TestEnvironment& env) { return env.fake_phyimpl_parent_.HasInitStaAddr(); }));
  EXPECT_EQ(driver_test().RunInEnvironmentTypeContext<uint16_t>(
                [](TestEnvironment& env) { return env.fake_phyimpl_parent_.kFakeIfaceId; }),
            result->value()->iface_id);
}

TEST_F(WlanphyDeviceTest, DestroyIface) {
  fuchsia_wlan_device::wire::DestroyIfaceRequest req = {
      .id = FakeWlanPhyImpl::kFakeIfaceId,
  };

  auto result = client_phy_->DestroyIface(std::move(req));
  ASSERT_TRUE(result.ok());
  WaitForCommandCompletion();
  EXPECT_EQ(driver_test().RunInEnvironmentTypeContext<uint16_t>(
                [](TestEnvironment& env) { return env.fake_phyimpl_parent_.kFakeIfaceId; }),
            req.id);
}

TEST_F(WlanphyDeviceTest, SetCountry) {
  fuchsia_wlan_device::wire::CountryCode country_code = {
      .alpha2 =
          {
              .data_ = {'U', 'S'},
          },
  };
  auto result = client_phy_->SetCountry(std::move(country_code));
  ASSERT_TRUE(result.ok());
  WaitForCommandCompletion();

  auto country =
      driver_test().RunInEnvironmentTypeContext<fuchsia_wlan_phyimpl::wire::WlanPhyCountry>(
          [](TestEnvironment& env) { return env.fake_phyimpl_parent_.GetCountryReq(); });
  EXPECT_EQ(0, memcmp(&country_code.alpha2.data()[0], &country.alpha2().data()[0],
                      fuchsia_wlan_phyimpl::wire::kWlanphyAlpha2Len));
}

TEST_F(WlanphyDeviceTest, GetCountry) {
  auto result = client_phy_->GetCountry();
  ASSERT_TRUE(result.ok());
  WaitForCommandCompletion();
  auto country_code =
      driver_test()
          .RunInEnvironmentTypeContext<
              fidl::Array<uint8_t, fuchsia_wlan_phyimpl::wire::kWlanphyAlpha2Len>>(
              [](TestEnvironment& env) { return env.fake_phyimpl_parent_.kAlpha2; });
  EXPECT_EQ(0, memcmp(&result->value()->resp.alpha2.data()[0], &country_code.data()[0],
                      fuchsia_wlan_phyimpl::wire::kWlanphyAlpha2Len));
}

TEST_F(WlanphyDeviceTest, ClearCountry) {
  auto result = client_phy_->ClearCountry();
  ASSERT_TRUE(result.ok());
  WaitForCommandCompletion();
}

TEST_F(WlanphyDeviceTest, SetPowerSaveMode) {
  fuchsia_wlan_common::wire::PowerSaveType ps_mode =
      fuchsia_wlan_common::wire::PowerSaveType::kPsModeLowPower;
  auto result = client_phy_->SetPowerSaveMode(std::move(ps_mode));
  ASSERT_TRUE(result.ok());
  WaitForCommandCompletion();
  EXPECT_EQ(driver_test().RunInEnvironmentTypeContext<fuchsia_wlan_common::wire::PowerSaveType>(
                [](TestEnvironment& env) { return env.fake_phyimpl_parent_.GetPSType(); }),
            ps_mode);
}

TEST_F(WlanphyDeviceTest, GetPowerSaveMode) {
  auto result = client_phy_->GetPowerSaveMode();
  ASSERT_TRUE(result.ok());
  WaitForCommandCompletion();
  EXPECT_EQ(driver_test().RunInEnvironmentTypeContext<fuchsia_wlan_common::wire::PowerSaveType>(
                [](TestEnvironment& env) { return env.fake_phyimpl_parent_.kFakePsMode; }),
            result->value()->resp);
}
}  // namespace
}  // namespace wlanphy
