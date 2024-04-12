// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.wlan.phyimpl/cpp/driver/wire.h>
#include <fuchsia/wlan/common/cpp/fidl.h>
#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/cpp/task.h>
#include <lib/async_patterns/testing/cpp/dispatcher_bound.h>
#include <lib/component/incoming/cpp/service.h>
#include <lib/driver/incoming/cpp/namespace.h>
#include <lib/driver/testing/cpp/driver_lifecycle.h>
#include <lib/driver/testing/cpp/driver_runtime.h>
#include <lib/driver/testing/cpp/test_environment.h>
#include <lib/driver/testing/cpp/test_node.h>
#include <lib/fdf/testing.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fidl/cpp/decoder.h>
#include <lib/fidl/cpp/message.h>
#include <lib/sync/cpp/completion.h>
#include <lib/sys/cpp/testing/component_context_provider.h>
#include <netinet/if_ether.h>
#include <zircon/errors.h>
#include <zircon/system/ulib/async-default/include/lib/async/default.h>

#include <gtest/gtest.h>

#include "fidl/fuchsia.wlan.phyimpl/cpp/wire_types.h"
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
    FDF_LOG(INFO, "***%s called***", __func__);
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

using fdf_testing::TestEnvironment;

class WlanphyDeviceTest : public ::testing::Test {
 public:
  void SetUp() override {
    // Create start args
    zx::result start_args = node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
    EXPECT_EQ(ZX_OK, start_args.status_value());

    // Start the test environment with incoming directory returned from the start args
    zx::result init_result = test_environment_.SyncCall(
        &TestEnvironment::Initialize, std::move(start_args->incoming_directory_server));
    EXPECT_EQ(ZX_OK, init_result.status_value());

    auto wlanphyimpl = [this](fdf::ServerEnd<fuchsia_wlan_phyimpl::WlanPhyImpl> server_end) {
      fake_phyimpl_parent_.SyncCall(&FakeWlanPhyImpl::ServiceConnectHandler, std::move(server_end));
    };

    // Add the service contains WlanPhyImpl protocol to outgoing directory.
    fuchsia_wlan_phyimpl::Service::InstanceHandler wlanphyimpl_service_handler(
        {.wlan_phy_impl = wlanphyimpl});

    test_environment_.SyncCall(
        [](TestEnvironment* env, fuchsia_wlan_phyimpl::Service::InstanceHandler&& handler) {
          zx::result result = env->incoming_directory().AddService<fuchsia_wlan_phyimpl::Service>(
              std::move(handler));
          ASSERT_TRUE(result.is_ok());
        },
        std::move(wlanphyimpl_service_handler));

    // Start the driver. This should setup the devfs connector as well.
    zx::result start_result = runtime_.RunToCompletion(driver_.SyncCall(
        &fdf_testing::DriverUnderTest<wlanphy::Device>::Start, std::move(start_args->start_args)));
    EXPECT_EQ(ZX_OK, start_result.status_value());

    // Connect to the service (device connector) provided by the devfs node
    zx::result conn_status = node_server_.SyncCall([](fdf_testing::TestNode* root_node) {
      return root_node->children().at("wlanphy").ConnectToDevice();
    });
    EXPECT_EQ(ZX_OK, conn_status.status_value());
    // Bind to the client end
    fidl::ClientEnd<fuchsia_wlan_device::Connector> client_end(std::move(conn_status.value()));
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
    DriverPrepareStop();
    test_environment_.reset();
    node_server_.reset();
    runtime_.ShutdownAllDispatchers(driver_dispatcher_->get());
  }

  void DriverPrepareStop() {
    zx::result prepare_stop_result = runtime_.RunToCompletion(
        driver_.SyncCall(&fdf_testing::DriverUnderTest<wlanphy::Device>::PrepareStop));
    EXPECT_EQ(ZX_OK, prepare_stop_result.status_value());
  }

  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<::wlanphy::Device>>& driver() {
    return driver_;
  }

  async_dispatcher_t* env_dispatcher() { return env_dispatcher_->async_dispatcher(); }
  async_dispatcher_t* phyimpl_dispatcher() { return phyimpl_dispatcher_->async_dispatcher(); }

  // Attaches a foreground dispatcher for us automatically.
  fdf_testing::DriverRuntime runtime_;

  // Env dispatcher runs in the background because we need to make sync calls into it.
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();
  // This dispatcher handles all the FakePhyImplParent related tasks.
  fdf::UnownedSynchronizedDispatcher phyimpl_dispatcher_ = runtime_.StartBackgroundDispatcher();
  fdf::UnownedSynchronizedDispatcher phy_client_dispatcher_ = runtime_.StartBackgroundDispatcher();
  fdf::UnownedSynchronizedDispatcher driver_dispatcher_ = runtime_.StartBackgroundDispatcher();

  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      env_dispatcher(), std::in_place, std::string("root")};

  async_patterns::TestDispatcherBound<TestEnvironment> test_environment_{env_dispatcher(),
                                                                         std::in_place};

  async_patterns::TestDispatcherBound<fdf_testing::DriverUnderTest<wlanphy::Device>> driver_{
      driver_dispatcher_->async_dispatcher(), std::in_place};
  async_patterns::TestDispatcherBound<FakeWlanPhyImpl> fake_phyimpl_parent_{phyimpl_dispatcher(),
                                                                            std::in_place};
  // The FIDL client to communicate with wlanphy device.
  fidl::WireSyncClient<fuchsia_wlan_device::Phy> client_phy_;
  fidl::WireSyncClient<fuchsia_wlan_device::Connector> phy_connector_;
};

TEST_F(WlanphyDeviceTest, GetSupportedMacRoles) {
  auto result = client_phy_->GetSupportedMacRoles();
  fake_phyimpl_parent_.SyncCall(&FakeWlanPhyImpl::WaitForCompletion);
  EXPECT_EQ(result.ok(), true);
  EXPECT_EQ(1U, result.value()->supported_mac_roles.count());
  EXPECT_EQ(FakeWlanPhyImpl::kFakeMacRole, result.value()->supported_mac_roles.data()[0]);
}

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
  fake_phyimpl_parent_.SyncCall(&FakeWlanPhyImpl::WaitForCompletion);
  EXPECT_FALSE(fake_phyimpl_parent_.SyncCall(&FakeWlanPhyImpl::HasInitStaAddr));
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
  fake_phyimpl_parent_.SyncCall(&FakeWlanPhyImpl::WaitForCompletion);
  EXPECT_TRUE(fake_phyimpl_parent_.SyncCall(&FakeWlanPhyImpl::HasInitStaAddr));
#if 0
  auto received_req = fake_phyimpl_parent_.SyncCall(&FakeWlanPhyImpl::GetIfaceReq);
  EXPECT_EQ(0, memcmp(received_req->dinit_sta_addr, &req.init_sta_addr.data()[0],
                      fuchsia_wlan_ieee80211::wire::kMacAddrLen));
#endif
  EXPECT_EQ(FakeWlanPhyImpl::kFakeIfaceId, result->value()->iface_id);
}

TEST_F(WlanphyDeviceTest, DestroyIface) {
  fuchsia_wlan_device::wire::DestroyIfaceRequest req = {
      .id = FakeWlanPhyImpl::kFakeIfaceId,
  };

  auto result = client_phy_->DestroyIface(std::move(req));
  ASSERT_TRUE(result.ok());
  fake_phyimpl_parent_.SyncCall(&FakeWlanPhyImpl::WaitForCompletion);
  EXPECT_EQ(req.id, FakeWlanPhyImpl::kFakeIfaceId);
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
  fake_phyimpl_parent_.SyncCall(&FakeWlanPhyImpl::WaitForCompletion);

  auto country = fake_phyimpl_parent_.SyncCall(&FakeWlanPhyImpl::GetCountryReq);
  EXPECT_EQ(0, memcmp(&country_code.alpha2.data()[0], &country.alpha2().data()[0],
                      fuchsia_wlan_phyimpl::wire::kWlanphyAlpha2Len));
}

TEST_F(WlanphyDeviceTest, GetCountry) {
  auto result = client_phy_->GetCountry();
  ASSERT_TRUE(result.ok());
  fake_phyimpl_parent_.SyncCall(&FakeWlanPhyImpl::WaitForCompletion);
  EXPECT_EQ(0, memcmp(&result->value()->resp.alpha2.data()[0], &FakeWlanPhyImpl::kAlpha2.data()[0],
                      fuchsia_wlan_phyimpl::wire::kWlanphyAlpha2Len));
}

TEST_F(WlanphyDeviceTest, ClearCountry) {
  auto result = client_phy_->ClearCountry();
  ASSERT_TRUE(result.ok());
  fake_phyimpl_parent_.SyncCall(&FakeWlanPhyImpl::WaitForCompletion);
}

TEST_F(WlanphyDeviceTest, SetPowerSaveMode) {
  fuchsia_wlan_common::wire::PowerSaveType ps_mode =
      fuchsia_wlan_common::wire::PowerSaveType::kPsModeLowPower;
  auto result = client_phy_->SetPowerSaveMode(std::move(ps_mode));
  ASSERT_TRUE(result.ok());
  fake_phyimpl_parent_.SyncCall(&FakeWlanPhyImpl::WaitForCompletion);
  EXPECT_EQ(fake_phyimpl_parent_.SyncCall(&FakeWlanPhyImpl::GetPSType), ps_mode);
}

TEST_F(WlanphyDeviceTest, GetPowerSaveMode) {
  auto result = client_phy_->GetPowerSaveMode();
  ASSERT_TRUE(result.ok());
  fake_phyimpl_parent_.SyncCall(&FakeWlanPhyImpl::WaitForCompletion);
  EXPECT_EQ(FakeWlanPhyImpl::kFakePsMode, result->value()->resp);
}
}  // namespace
}  // namespace wlanphy
