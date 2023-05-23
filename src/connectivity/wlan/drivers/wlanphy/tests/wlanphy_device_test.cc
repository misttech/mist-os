// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.wlan.phyimpl/cpp/driver/wire.h>
#include <fuchsia/wlan/common/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/sync/cpp/completion.h>

#include <zxtest/zxtest.h>

#include "fidl/fuchsia.wlan.phyimpl/cpp/wire_types.h"
#include "src/connectivity/wlan/drivers/wlanphy/device.h"
#include "src/connectivity/wlan/drivers/wlanphy/driver.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

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
// TODO(fxb/124464): Migrate test to use dispatcher integration.
class WlanphyDeviceTest : public ::zxtest::Test,
                          public fdf::WireServer<fuchsia_wlan_phyimpl::WlanPhyImpl> {
 public:
  WlanphyDeviceTest()
      : client_loop_phy_(async::Loop(&kAsyncLoopConfigNoAttachToCurrentThread)),
        fake_wlan_phy_impl_device_(MockDevice::FakeRootParentNoDispatcherIntegrationDEPRECATED()) {
    zx_status_t status = ZX_OK;

    // Create end points for the protocol fuchsia_wlan_device::Phy, these two end points will be
    // bind with WlanphyDeviceTest(as fake wlandevicemonitor) and wlanphy device.
    auto endpoints_phy = fidl::CreateEndpoints<fuchsia_wlan_device::Phy>();
    ASSERT_FALSE(endpoints_phy.is_error());

    // Create end points for the protocol fuchsia_wlan_phyimpl::WlanPhyImpl, these two end
    // points will be bind with wlanphy device and WlanphyDeviceTest(as fake WlanPhyImplDevice).
    auto endpoints_phy_impl = fdf::CreateEndpoints<fuchsia_wlan_phyimpl::WlanPhyImpl>();
    ASSERT_FALSE(endpoints_phy_impl.is_error());

    status = client_loop_phy_.StartThread("fake-wlandevicemonitor-loop");
    // There is a return value in ASSERT_EQ(), cannot apply here.
    EXPECT_EQ(ZX_OK, status);

    // Cache the client dispatcher of fuchsia_wlan_device::Phy protocol.
    client_dispatcher_phy_ = client_loop_phy_.dispatcher();

    // Create the client for the protocol fuchsia_wlan_device::Phy based on dispatcher end
    // point.
    client_phy_ = fidl::WireSharedClient<fuchsia_wlan_device::Phy>(std::move(endpoints_phy->client),
                                                                   client_dispatcher_phy_);

    libsync::Completion wlanphy_created;
    async::PostTask(driver_dispatcher_.dispatcher(), [&]() {
      wlanphy_device_ =
          new Device(fake_wlan_phy_impl_device_.get(), std::move(endpoints_phy_impl->client));

      EXPECT_EQ(ZX_OK, wlanphy_device_->DeviceAdd());

      // Establish FIDL connection based on fuchsia_wlan_phyimpl::WlanPhyImpl protocol.
      // Call DdkAsyncRemove() here to make ReleaseFlaggedDevices fully executed. This function will
      // stop the device from working properly, so we can call it anywhere before
      // ReleaseFlaggedDevices() is called. We just call it as soon as the device is created here.
      wlanphy_device_->DdkAsyncRemove();

      // Establish FIDL connection based on fuchsia_wlan_device::Phy protocol.
      wlanphy_device_->Connect(std::move(endpoints_phy->server));

      wlanphy_created.Signal();
    });

    status = wlanphy_created.Wait(zx::sec(10));
    EXPECT_OK(status);
    EXPECT_NOT_NULL(wlanphy_device_);

    fdf::BindServer(server_dispatcher_phy_impl_.driver_dispatcher().get(),
                    std::move(endpoints_phy_impl->server), this);

    // Initialize struct to avoid random values.
    memset(static_cast<void*>(&create_iface_req_), 0, sizeof(create_iface_req_));
  }

  ~WlanphyDeviceTest() {
    // Mock DDK will delete wlanphy_device_.
    mock_ddk::ReleaseFlaggedDevices(wlanphy_device_->zxdev());
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
  static constexpr fidl::Array<uint8_t, WLANPHY_ALPHA2_LEN> kAlpha2{'W', 'W'};
  static constexpr fuchsia_wlan_common::wire::PowerSaveType kFakePsMode =
      fuchsia_wlan_common::wire::PowerSaveType::kPsModePerformance;
  static constexpr ::fidl::Array<uint8_t, 6> kValidStaAddr = {1, 2, 3, 4, 5, 6};
  static constexpr ::fidl::Array<uint8_t, 6> kInvalidStaAddr = {0, 0, 0, 0, 0, 0};

  // The completion to synchronize the state in tests, because there are async FIDL calls.
  libsync::Completion test_completion_;

 protected:
  // The FIDL client to communicate with wlanphy device.
  fidl::WireSharedClient<fuchsia_wlan_device::Phy> client_phy_;

  void* dummy_ctx_;

 private:
  fdf_testing::DriverRuntimeEnv managed_env_;
  async::Loop client_loop_phy_;

  // Dispatcher for the FIDL client sending requests to wlanphy device.
  async_dispatcher_t* client_dispatcher_phy_;

  // Dispatcher for being a driver transport FIDL server to receive and dispatch requests from
  // wlanphy device.
  fdf::TestSynchronizedDispatcher server_dispatcher_phy_impl_{fdf::kDispatcherManaged};
  // The driver dispatcher used for testing.
  fdf::TestSynchronizedDispatcher driver_dispatcher_{fdf::kDispatcherManaged};

  // fake zx_device as the the parent of wlanphy device.
  std::shared_ptr<MockDevice> fake_wlan_phy_impl_device_;
  Device* wlanphy_device_;
};

TEST_F(WlanphyDeviceTest, GetSupportedMacRoles) {
  auto result = client_phy_.sync()->GetSupportedMacRoles();
  ASSERT_TRUE(result.ok());
  test_completion_.Wait();

  EXPECT_EQ(1U, result->value()->supported_mac_roles.count());
  EXPECT_EQ(kFakeMacRole, result->value()->supported_mac_roles.data()[0]);
}

TEST_F(WlanphyDeviceTest, CreateIfaceTestNullAddr) {
  auto dummy_ends = fidl::CreateEndpoints<fuchsia_wlan_device::Phy>();
  auto dummy_channel = dummy_ends->server.TakeChannel();
  // All-zero MAC address in the request will should result in a false on has_init_sta_addr in
  // next level's FIDL request.
  fuchsia_wlan_device::wire::CreateIfaceRequest req = {
      .role = fuchsia_wlan_common::wire::WlanMacRole::kClient,
      .mlme_channel = std::move(dummy_channel),
      .init_sta_addr = kInvalidStaAddr,
  };

  auto result = client_phy_.sync()->CreateIface(std::move(req));
  ASSERT_TRUE(result.ok());
  test_completion_.Wait();
  EXPECT_FALSE(has_init_sta_addr_);
}

TEST_F(WlanphyDeviceTest, CreateIfaceTestValidAddr) {
  auto dummy_ends = fidl::CreateEndpoints<fuchsia_wlan_device::Phy>();
  auto dummy_channel = dummy_ends->server.TakeChannel();

  fuchsia_wlan_device::wire::CreateIfaceRequest req = {
      .role = fuchsia_wlan_common::wire::WlanMacRole::kClient,
      .mlme_channel = std::move(dummy_channel),
      .init_sta_addr = kValidStaAddr,
  };

  auto result = client_phy_.sync()->CreateIface(std::move(req));
  ASSERT_TRUE(result.ok());
  test_completion_.Wait();
  EXPECT_TRUE(has_init_sta_addr_);
  EXPECT_EQ(0, memcmp(&create_iface_req_.init_sta_addr, &req.init_sta_addr.data()[0],
                      fuchsia_wlan_ieee80211::wire::kMacAddrLen));
  EXPECT_EQ(kFakeIfaceId, result->value()->iface_id);
}

TEST_F(WlanphyDeviceTest, DestroyIface) {
  fuchsia_wlan_device::wire::DestroyIfaceRequest req = {
      .id = kFakeIfaceId,
  };

  auto result = client_phy_.sync()->DestroyIface(std::move(req));
  ASSERT_TRUE(result.ok());
  test_completion_.Wait();
  EXPECT_EQ(req.id, destroy_iface_id_);
}

TEST_F(WlanphyDeviceTest, SetCountry) {
  fuchsia_wlan_device::wire::CountryCode country_code = {
      .alpha2 =
          {
              .data_ = {'U', 'S'},
          },
  };
  auto result = client_phy_.sync()->SetCountry(std::move(country_code));
  ASSERT_TRUE(result.ok());
  test_completion_.Wait();

  EXPECT_EQ(0, memcmp(&country_code.alpha2.data()[0], &country_.alpha2().data()[0],
                      fuchsia_wlan_phyimpl::wire::kWlanphyAlpha2Len));
}

TEST_F(WlanphyDeviceTest, GetCountry) {
  auto result = client_phy_.sync()->GetCountry();
  ASSERT_TRUE(result.ok());
  test_completion_.Wait();

  EXPECT_EQ(0, memcmp(&result->value()->resp.alpha2.data()[0], &kAlpha2.data()[0],
                      fuchsia_wlan_phyimpl::wire::kWlanphyAlpha2Len));
}

TEST_F(WlanphyDeviceTest, ClearCountry) {
  auto result = client_phy_.sync()->ClearCountry();
  ASSERT_TRUE(result.ok());
  test_completion_.Wait();
}

TEST_F(WlanphyDeviceTest, SetPowerSaveMode) {
  fuchsia_wlan_common::wire::PowerSaveType ps_mode =
      fuchsia_wlan_common::wire::PowerSaveType::kPsModeLowPower;
  auto result = client_phy_.sync()->SetPowerSaveMode(std::move(ps_mode));
  ASSERT_TRUE(result.ok());
  test_completion_.Wait();

  EXPECT_EQ(ps_mode_, ps_mode);
}

TEST_F(WlanphyDeviceTest, GetPowerSaveMode) {
  auto result = client_phy_.sync()->GetPowerSaveMode();
  ASSERT_TRUE(result.ok());
  test_completion_.Wait();

  EXPECT_EQ(kFakePsMode, result->value()->resp);
}
}  // namespace
}  // namespace wlanphy
