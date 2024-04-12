// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/wlan/drivers/wlanif/device.h"

#include <fuchsia/wlan/fullmac/c/banjo.h>
#include <fuchsia/wlan/ieee80211/cpp/fidl.h>
#include <fuchsia/wlan/mlme/cpp/fidl.h>
#include <fuchsia/wlan/mlme/cpp/fidl_test_base.h>
#include <fuchsia/wlan/sme/cpp/fidl.h>
#include <fuchsia/wlan/sme/cpp/fidl_test_base.h>
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

#include <memory>

#include <gtest/gtest.h>

#include "fidl/fuchsia.wlan.fullmac/cpp/wire_types.h"
#include "fuchsia/wlan/fullmac/c/banjo.h"
#include "src/connectivity/wlan/drivers/wlanif/test/test_bss.h"
#include "src/connectivity/wlan/lib/common/cpp/include/wlan/common/macaddr.h"
#include "src/connectivity/wlan/lib/mlme/fullmac/c-binding/bindings.h"

constexpr zx::duration kWaitForCallbackDuration = zx::msec(1000);
namespace {

constexpr uint8_t kPeerStaAddress[ETH_ALEN] = {0xba, 0xbe, 0xfa, 0xce, 0x00, 0x00};

std::pair<zx::channel, zx::channel> make_channel() {
  zx::channel local;
  zx::channel remote;
  zx::channel::create(0, &local, &remote);
  return {std::move(local), std::move(remote)};
}

rust_wlan_fullmac_ifc_protocol_ops_copy_t EmptyRustProtoOps() {
  return rust_wlan_fullmac_ifc_protocol_ops_copy_t{
      .on_scan_result = [](void* ctx, const wlan_fullmac_scan_result_t* result) {},
      .on_scan_end = [](void* ctx, const wlan_fullmac_scan_end_t* end) {},
      .connect_conf = [](void* ctx, const wlan_fullmac_connect_confirm_t* resp) {},
      .roam_conf = [](void* ctx, const wlan_fullmac_roam_confirm_t* resp) {},
      .auth_ind = [](void* ctx, const wlan_fullmac_auth_ind_t* ind) {},
      .deauth_conf = [](void* ctx, const uint8_t peer_sta_address[ETH_ALEN]) {},
      .deauth_ind = [](void* ctx, const wlan_fullmac_deauth_indication_t* ind) {},
      .assoc_ind = [](void* ctx, const wlan_fullmac_assoc_ind_t* ind) {},
      .disassoc_conf = [](void* ctx, const wlan_fullmac_disassoc_confirm_t* resp) {},
      .disassoc_ind = [](void* ctx, const wlan_fullmac_disassoc_indication_t* ind) {},
      .start_conf = [](void* ctx, const wlan_fullmac_start_confirm_t* resp) {},
      .stop_conf = [](void* ctx, const wlan_fullmac_stop_confirm_t* resp) {},
      .eapol_conf = [](void* ctx, const wlan_fullmac_eapol_confirm_t* resp) {},
      .on_channel_switch = [](void* ctx, const wlan_fullmac_channel_switch_info_t* resp) {},
      .signal_report = [](void* ctx, const wlan_fullmac_signal_report_indication_t* ind) {},
      .eapol_ind = [](void* ctx, const wlan_fullmac_eapol_indication_t* ind) {},
      .on_pmk_available = [](void* ctx, const wlan_fullmac_pmk_info_t* info) {},
      .sae_handshake_ind = [](void* ctx, const wlan_fullmac_sae_handshake_ind_t* ind) {},
      .sae_frame_rx = [](void* ctx, const wlan_fullmac_sae_frame_t* frame) {},
      .on_wmm_status_resp = [](void* ctx, zx_status_t status,
                               const wlan_wmm_parameters_t* wmm_params) {},
  };
}

class FakeFullmacParent : public fdf::WireServer<fuchsia_wlan_fullmac::WlanFullmacImpl> {
 public:
  void ServiceConnectHandler(fdf::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> server_end) {
    fdf::BindServer(fdf_dispatcher_get_current_dispatcher(), std::move(server_end), this);
  }
  FakeFullmacParent() { memcpy(peer_sta_addr_, kPeerStaAddress, ETH_ALEN); }
  void Start(StartRequestView request, fdf::Arena& arena,
             StartCompleter::Sync& completer) override {
    client_ =
        fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImplIfc>(std::move(request->ifc));

    // Create and send a mock channel up here since the sme_channel field is required in the reply
    // of Start().
    auto [local, remote] = make_channel();
    completer.buffer(arena).ReplySuccess(std::move(local));
  }
  void Stop(fdf::Arena& arena, StopCompleter::Sync& completer) override {}
  void Query(fdf::Arena& arena, QueryCompleter::Sync& completer) override {
    fuchsia_wlan_fullmac::wire::WlanFullmacQueryInfo info = {};
    info.role = fuchsia_wlan_common::wire::WlanMacRole::kClient;
    info.band_cap_count = 0;
    completer.buffer(arena).ReplySuccess(info);
  }
  void QueryMacSublayerSupport(fdf::Arena& arena,
                               QueryMacSublayerSupportCompleter::Sync& completer) override {
    fuchsia_wlan_common::wire::MacSublayerSupport mac_sublayer_support;
    mac_sublayer_support.data_plane.data_plane_type = data_plane_type_;
    mac_sublayer_support.device.mac_implementation_type =
        fuchsia_wlan_common::wire::MacImplementationType::kFullmac;
    completer.buffer(arena).ReplySuccess(mac_sublayer_support);
  }
  void QuerySecuritySupport(fdf::Arena& arena,
                            QuerySecuritySupportCompleter::Sync& completer) override {}
  void QuerySpectrumManagementSupport(
      fdf::Arena& arena, QuerySpectrumManagementSupportCompleter::Sync& completer) override {}
  void StartScan(StartScanRequestView request, fdf::Arena& arena,
                 StartScanCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_scan_type(), true);
    EXPECT_EQ(request->has_channels(), true);
    EXPECT_EQ(request->has_min_channel_time(), true);
    EXPECT_EQ(request->has_max_channel_time(), true);
    completer.buffer(arena).Reply();
  }
  void Connect(ConnectRequestView request, fdf::Arena& arena,
               ConnectCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_selected_bss(), true);
    EXPECT_EQ(request->has_auth_type(), true);
    EXPECT_EQ(request->has_connect_failure_timeout(), true);
    EXPECT_EQ(request->has_security_ie(), true);
    completer.buffer(arena).Reply();
  }
  void Reconnect(ReconnectRequestView request, fdf::Arena& arena,
                 ReconnectCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_peer_sta_address(), true);
    completer.buffer(arena).Reply();
  }
  void AuthResp(AuthRespRequestView request, fdf::Arena& arena,
                AuthRespCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_peer_sta_address(), true);
    EXPECT_EQ(request->has_result_code(), true);
    completer.buffer(arena).Reply();
  }
  void Deauth(DeauthRequestView request, fdf::Arena& arena,
              DeauthCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_peer_sta_address(), true);
    EXPECT_EQ(request->has_reason_code(), true);
    completer.buffer(arena).Reply();
  }
  void AssocResp(AssocRespRequestView request, fdf::Arena& arena,
                 AssocRespCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_peer_sta_address(), true);
    EXPECT_EQ(request->has_result_code(), true);
    EXPECT_EQ(request->has_association_id(), true);
    completer.buffer(arena).Reply();
  }
  void Disassoc(DisassocRequestView request, fdf::Arena& arena,
                DisassocCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_peer_sta_address(), true);
    EXPECT_EQ(request->has_reason_code(), true);
    completer.buffer(arena).Reply();
  }
  void Reset(ResetRequestView request, fdf::Arena& arena,
             ResetCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_sta_address(), true);
    EXPECT_EQ(request->has_set_default_mib(), true);
    completer.buffer(arena).Reply();
  }
  void StartBss(StartBssRequestView request, fdf::Arena& arena,
                StartBssCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_beacon_period(), true);
    EXPECT_EQ(request->has_bss_type(), true);
    EXPECT_EQ(request->has_channel(), true);
    EXPECT_EQ(request->has_dtim_period(), true);
    EXPECT_EQ(request->has_ssid(), true);
    completer.buffer(arena).Reply();
  }
  void StopBss(StopBssRequestView request, fdf::Arena& arena,
               StopBssCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_ssid(), true);
    completer.buffer(arena).Reply();
  }
  void SetKeysReq(SetKeysReqRequestView request, fdf::Arena& arena,
                  SetKeysReqCompleter::Sync& completer) override {
    EXPECT_GE(request->req.num_keys, 1ul);
    fuchsia_wlan_fullmac::wire::WlanFullmacSetKeysResp resp = {};
    resp.num_keys = request->req.num_keys;
    completer.buffer(arena).Reply(resp);
  }
  void DelKeysReq(DelKeysReqRequestView request, fdf::Arena& arena,
                  DelKeysReqCompleter::Sync& completer) override {
    EXPECT_GE(request->req.num_keys, 1ul);
    del_key_request_received_ = true;
    completer.buffer(arena).Reply();
  }
  void EapolTx(EapolTxRequestView request, fdf::Arena& arena,
               EapolTxCompleter::Sync& completer) override {
    EXPECT_GE(request->data().count(), 1ul);
    eapol_tx_request_received_ = true;
    completer.buffer(arena).Reply();
  }
  void GetIfaceCounterStats(fdf::Arena& arena,
                            GetIfaceCounterStatsCompleter::Sync& completer) override {}
  void GetIfaceHistogramStats(fdf::Arena& arena,
                              GetIfaceHistogramStatsCompleter::Sync& completer) override {}
  void SetMulticastPromisc(SetMulticastPromiscRequestView request, fdf::Arena& arena,
                           SetMulticastPromiscCompleter::Sync& completer) override {}
  void SaeHandshakeResp(SaeHandshakeRespRequestView request, fdf::Arena& arena,
                        SaeHandshakeRespCompleter::Sync& completer) override {}
  void SaeFrameTx(SaeFrameTxRequestView request, fdf::Arena& arena,
                  SaeFrameTxCompleter::Sync& completer) override {}
  void WmmStatusReq(fdf::Arena& arena, WmmStatusReqCompleter::Sync& completer) override {}
  void OnLinkStateChanged(OnLinkStateChangedRequestView request, fdf::Arena& arena,
                          OnLinkStateChangedCompleter::Sync& completer) override {
    EXPECT_EQ(request->online, expected_online_);
    link_state_changed_called_ = true;
    completer.buffer(arena).Reply();
  }

  void SendDeauthConf() {
    fidl::Array<uint8_t, ETH_ALEN> peer_sta_address;
    memcpy(peer_sta_address.data(), peer_sta_addr_, ETH_ALEN);

    auto req = fuchsia_wlan_fullmac::wire::WlanFullmacImplIfcBaseDeauthConfRequest::Builder(arena_)
                   .peer_sta_address(peer_sta_address)
                   .Build();
    auto result = client_.buffer(arena_)->DeauthConf(req);
    EXPECT_TRUE(result.ok());
  }

  void SendScanResult() {
    fuchsia_wlan_fullmac::wire::WlanFullmacScanResult scan_result = {};
    scan_result.bss.bss_type = fuchsia_wlan_common::wire::BssType::kInfrastructure;
    scan_result.bss.channel.primary = 9;
    scan_result.bss.channel.cbw = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw20;

    auto result = client_.buffer(arena_)->OnScanResult(scan_result);
    EXPECT_TRUE(result.ok());
  }

  void SendScanEnd() {
    fuchsia_wlan_fullmac::wire::WlanFullmacScanEnd scan_end = {};
    scan_end.code = fuchsia_wlan_fullmac::WlanScanResult::kSuccess;
    auto result = client_.buffer(arena_)->OnScanEnd(scan_end);
    EXPECT_TRUE(result.ok());
  }

  void SendConnectConf() {
    fuchsia_wlan_fullmac::wire::WlanFullmacConnectConfirm conf = {};
    conf.result_code = fuchsia_wlan_ieee80211::wire::StatusCode::kSuccess;
    auto result = client_.buffer(arena_)->ConnectConf(conf);
    EXPECT_TRUE(result.ok());
  }

  void SendRoamConf() {
    fuchsia_wlan_fullmac::wire::WlanFullmacRoamConfirm conf = {};
    conf.result_code = fuchsia_wlan_ieee80211::wire::StatusCode::kSuccess;
    conf.selected_bss.bss_type = fuchsia_wlan_common::wire::BssType::kInfrastructure;
    conf.selected_bss.channel.primary = 9;
    conf.selected_bss.channel.cbw = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw20;
    conf.selected_bss.channel.cbw = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw20;
    auto result = client_.buffer(arena_)->RoamConf(conf);
    EXPECT_TRUE(result.ok());
  }

  void SendAuthInd() {
    fuchsia_wlan_fullmac::wire::WlanFullmacAuthInd ind = {};
    ind.auth_type = fuchsia_wlan_fullmac::wire::WlanAuthType::kOpenSystem;
    auto result = client_.buffer(arena_)->AuthInd(ind);
    EXPECT_TRUE(result.ok());
  }

  void SendDeauthInd() {
    fuchsia_wlan_fullmac::wire::WlanFullmacDeauthIndication ind = {};
    ind.reason_code = fuchsia_wlan_ieee80211::wire::ReasonCode::kUnspecifiedReason;
    auto result = client_.buffer(arena_)->DeauthInd(ind);
    EXPECT_TRUE(result.ok());
  }

  void SendAssocInd() {
    fuchsia_wlan_fullmac::wire::WlanFullmacAssocInd ind = {};
    auto result = client_.buffer(arena_)->AssocInd(ind);
    EXPECT_TRUE(result.ok());
  }

  void SendDisassocInd() {
    fuchsia_wlan_fullmac::wire::WlanFullmacDisassocIndication ind = {};
    auto result = client_.buffer(arena_)->DisassocInd(ind);
    EXPECT_TRUE(result.ok());
  }

  void SetDataPlaneType(fuchsia_wlan_common::wire::DataPlaneType data_plane_type) {
    data_plane_type_ = data_plane_type;
  }

  void SetExpectedOnline(bool value) { expected_online_ = value; }

  void SetLinkStateChanged(bool value) { link_state_changed_called_ = value; }

  bool GetLinkStateChanged() { return link_state_changed_called_; }
  bool GetDelKeyRequestReceived() { return del_key_request_received_; }
  bool GetEapolTxRequestReceived() { return eapol_tx_request_received_; }

  // Client to fire WlanFullmacImplIfc FIDL requests to wlanif driver.
  fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImplIfc> client_;

  fuchsia_wlan_common::wire::DataPlaneType data_plane_type_ =
      fuchsia_wlan_common::wire::DataPlaneType::kGenericNetworkDevice;
  bool expected_online_ = false;
  bool link_state_changed_called_ = false;
  bool del_key_request_received_ = false;
  bool eapol_tx_request_received_ = false;

 private:
  fdf::Arena arena_ = fdf::Arena::Create(0, 0).value();
  uint8_t peer_sta_addr_[ETH_ALEN];
};

using fdf_testing::TestEnvironment;

class WlanifDeviceTest : public ::testing::Test {
 public:
  void SetUp() override {
    // Create start args
    zx::result start_args = node_server_.SyncCall(&fdf_testing::TestNode::CreateStartArgsAndServe);
    EXPECT_EQ(ZX_OK, start_args.status_value());

    // Start the test environment with incoming directory returned from the start args
    zx::result init_result =
        test_environment_.SyncCall(&fdf_testing::TestEnvironment::Initialize,
                                   std::move(start_args->incoming_directory_server));
    EXPECT_EQ(ZX_OK, init_result.status_value());

    auto wlanfullmacimpl =
        [this](fdf::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> server_end) {
          fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::ServiceConnectHandler,
                                            std::move(server_end));
        };

    // Add the service contains WlanFullmac protocol to outgoing directory.
    fuchsia_wlan_fullmac::Service::InstanceHandler wlanfullmac_service_handler(
        {.wlan_fullmac_impl = wlanfullmacimpl});

    test_environment_.SyncCall(
        [](TestEnvironment* env, fuchsia_wlan_fullmac::Service::InstanceHandler&& handler) {
          zx::result result = env->incoming_directory().AddService<fuchsia_wlan_fullmac::Service>(
              std::move(handler));
          ASSERT_TRUE(result.is_ok());
        },
        std::move(wlanfullmac_service_handler));

    zx::result start_result =
        runtime_.RunToCompletion(driver_.Start(std::move(start_args->start_args)));
    EXPECT_EQ(ZX_OK, start_result.status_value());
  }

  void TearDown() override {
    DriverPrepareStop();
    test_environment_.reset();
    node_server_.reset();
    runtime_.ShutdownAllDispatchers(fdf::Dispatcher::GetCurrent()->get());
    ASSERT_EQ(ZX_OK, driver_.Stop().status_value());
  }

  void DriverPrepareStop() {
    zx::result prepare_stop_result = runtime_.RunToCompletion(driver_.PrepareStop());
    EXPECT_EQ(ZX_OK, prepare_stop_result.status_value());
  }

  zx_status_t StartWlanifDevice(const rust_wlan_fullmac_ifc_protocol_ops_copy_t* ops, void* ctx) {
    const rust_wlan_fullmac_ifc_protocol_copy_t proto{
        .ops = ops,
        .ctx = ctx,
    };

    zx::channel out_sme_channel;
    zx_status_t status = driver_->StartFullmac(&proto, &out_sme_channel);
    EXPECT_EQ(status, ZX_OK);
    return status;
  }

  fdf_testing::DriverUnderTest<::wlanif::Device>& driver() { return driver_; }

  async_dispatcher_t* env_dispatcher() { return env_dispatcher_->async_dispatcher(); }
  async_dispatcher_t* fullmac_dispatcher() { return fullmac_dispatcher_->async_dispatcher(); }

  // Attaches a foreground dispatcher for us automatically.
  fdf_testing::DriverRuntime runtime_;

  // Env dispatcher runs in the background because we need to make sync calls into it.
  fdf::UnownedSynchronizedDispatcher env_dispatcher_ = runtime_.StartBackgroundDispatcher();
  // This dispatcher handles all the FakeFullmacParent related tasks.
  fdf::UnownedSynchronizedDispatcher fullmac_dispatcher_ = runtime_.StartBackgroundDispatcher();

  async_patterns::TestDispatcherBound<fdf_testing::TestNode> node_server_{
      env_dispatcher(), std::in_place, std::string("root")};

  async_patterns::TestDispatcherBound<TestEnvironment> test_environment_{env_dispatcher(),
                                                                         std::in_place};

  async_patterns::TestDispatcherBound<FakeFullmacParent> fake_wlanfullmac_parent_{
      fullmac_dispatcher(), std::in_place};
  fdf_testing::DriverUnderTest<wlanif::Device> driver_;
};

TEST_F(WlanifDeviceTest, Basic) {
  auto ops = EmptyRustProtoOps();
  EXPECT_EQ(ZX_OK, StartWlanifDevice(&ops, nullptr));
}

TEST_F(WlanifDeviceTest, EthDataPlaneNotSupported) {
  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SetDataPlaneType,
                                    fuchsia_wlan_common::wire::DataPlaneType::kEthernetDevice);
  ASSERT_EQ(driver()->Bind(), ZX_ERR_NOT_SUPPORTED);
}

TEST_F(WlanifDeviceTest, OnLinkStateChangedOnlyCalledWhenStateChanges) {
  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SetExpectedOnline, true);
  driver()->OnLinkStateChanged(true);
  EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::GetLinkStateChanged));

  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SetLinkStateChanged, false);
  driver()->OnLinkStateChanged(true);
  EXPECT_FALSE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::GetLinkStateChanged));

  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SetLinkStateChanged, false);
  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SetExpectedOnline, false);
  driver()->OnLinkStateChanged(false);
  EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::GetLinkStateChanged));

  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SetLinkStateChanged, false);
  driver()->OnLinkStateChanged(false);
  EXPECT_FALSE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::GetLinkStateChanged));

  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SetLinkStateChanged, false);
  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SetExpectedOnline, true);
  driver()->OnLinkStateChanged(true);
  EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::GetLinkStateChanged));
}

TEST_F(WlanifDeviceTest, StartScan) {
  const uint8_t chan_list[1] = {0x9};
  const wlan_fullmac_impl_base_start_scan_request_t req = {.scan_type = WLAN_SCAN_TYPE_ACTIVE,
                                                           .channels_list = chan_list,
                                                           .channels_count = 1,
                                                           .ssids_count = 0,
                                                           .min_channel_time = 10,
                                                           .max_channel_time = 100};
  driver()->StartScan(&req);
}

TEST_F(WlanifDeviceTest, Connect) {
  const wlan_fullmac_impl_base_connect_request_t req = wlan_fullmac_test::CreateConnectReq();
  driver()->Connect(&req);
}

TEST_F(WlanifDeviceTest, CheckReconnReq) {
  wlan_fullmac_impl_base_reconnect_request_t req;
  memcpy(req.peer_sta_address, kPeerStaAddress, sizeof(kPeerStaAddress));
  driver()->Reconnect(&req);
}

TEST_F(WlanifDeviceTest, CheckDeauthReq) {
  wlan_fullmac_impl_base_deauth_request_t req = {.reason_code = REASON_CODE_AP_INITIATED};
  memcpy(req.peer_sta_address, kPeerStaAddress, sizeof(kPeerStaAddress));
  driver()->Deauthenticate(&req);
}

TEST_F(WlanifDeviceTest, CheckAuthResp) {
  wlan_fullmac_impl_base_auth_resp_request_t resp = {.result_code = WLAN_AUTH_RESULT_SUCCESS};
  memcpy(resp.peer_sta_address, kPeerStaAddress, sizeof(kPeerStaAddress));
  driver()->AuthenticateResp(&resp);
}

TEST_F(WlanifDeviceTest, CheckAssocResp) {
  wlan_fullmac_impl_base_assoc_resp_request resp = {.result_code = WLAN_ASSOC_RESULT_SUCCESS,
                                                    .association_id = 42};
  memcpy(resp.peer_sta_address, kPeerStaAddress, sizeof(kPeerStaAddress));
  driver()->AssociateResp(&resp);
}

TEST_F(WlanifDeviceTest, CheckDisassoc) {
  wlan_fullmac_impl_base_disassoc_request_t req = {.reason_code =
                                                       REASON_CODE_LEAVING_NETWORK_DISASSOC};
  memcpy(req.peer_sta_address, kPeerStaAddress, sizeof(kPeerStaAddress));
  driver()->Disassociate(&req);
}

TEST_F(WlanifDeviceTest, CheckStartBss) {
  wlan_fullmac_impl_base_start_bss_request_t req = {
      .bss_type = BSS_TYPE_INFRASTRUCTURE, .beacon_period = 100, .dtim_period = 100, .channel = 1};
  memcpy(req.ssid.data, kPeerStaAddress, sizeof(kPeerStaAddress));
  req.ssid.len = sizeof(kPeerStaAddress);
  driver()->StartBss(&req);
}

TEST_F(WlanifDeviceTest, CheckStopBss) {
  wlan_fullmac_impl_base_stop_bss_request_t req;
  memcpy(req.ssid.data, kPeerStaAddress, sizeof(kPeerStaAddress));
  req.ssid.len = sizeof(kPeerStaAddress);
  driver()->StopBss(&req);
}

TEST_F(WlanifDeviceTest, CheckReset) {
  wlan_fullmac_impl_base_reset_request_t req = {.set_default_mib = true};
  memcpy(req.sta_address, kPeerStaAddress, sizeof(kPeerStaAddress));
  driver()->Reset(&req);
}

TEST_F(WlanifDeviceTest, CheckDeauthConf) {
  libsync::Completion signal;
  auto ops = EmptyRustProtoOps();

  ops.deauth_conf = [](void* ctx, const uint8_t peer_sta_address[ETH_ALEN]) {
    for (size_t i = 0; i < ETH_ALEN; i++) {
      EXPECT_EQ(kPeerStaAddress[i], peer_sta_address[i]);
    }

    EXPECT_NE(ctx, nullptr);
    libsync::Completion* signal = static_cast<libsync::Completion*>(ctx);
    signal->Signal();
  };

  EXPECT_EQ(ZX_OK, StartWlanifDevice(&ops, &signal));
  wlan_fullmac_impl_base_deauth_request_t req;
  memcpy(req.peer_sta_address, kPeerStaAddress, ETH_ALEN);
  req.reason_code = 0;
  driver()->Deauthenticate(&req);
  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendDeauthConf);
  zx_status_t status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  driver()->Stop();
}

TEST_F(WlanifDeviceTest, SetDelKey) {
  wlan_fullmac_set_keys_req_t req = {};
  req.num_keys = 1;
  req.keylist[0].protection = WLAN_PROTECTION_RX_TX;
  req.keylist[0].cipher_type = CIPHER_SUITE_TYPE_USE_GROUP;
  req.keylist[0].key_type = WLAN_KEY_TYPE_PAIRWISE;
  req.keylist[0].key_idx = 0;
  wlan_fullmac_set_keys_resp_t resp = {};
  driver()->SetKeysReq(&req, &resp);
  EXPECT_EQ(resp.num_keys, 1u);

  wlan_fullmac_del_keys_req_t del_req = {};
  del_req.num_keys = 1;
  del_req.keylist[0].key_id = 0;
  del_req.keylist[0].key_type = WLAN_KEY_TYPE_PAIRWISE;
  driver()->DeleteKeysReq(&del_req);
  EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::GetDelKeyRequestReceived));
}

TEST_F(WlanifDeviceTest, CheckEapolTx) {
  wlan_fullmac_impl_base_eapol_tx_request_t req = {};
  uint8_t data[256];
  req.data_count = 100;
  req.data_list = data;
  driver()->EapolTx(&req);
  EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::GetEapolTxRequestReceived));
}

TEST_F(WlanifDeviceTest, CheckQueryInfo) {
  wlan_fullmac_query_info_t resp;
  driver()->QueryDeviceInfo(&resp);
  EXPECT_EQ(resp.role, WLAN_MAC_ROLE_CLIENT);
}

TEST_F(WlanifDeviceTest, CheckMacLayerSupport) {
  fake_wlanfullmac_parent_.SyncCall(
      &FakeFullmacParent::SetDataPlaneType,
      fuchsia_wlan_common::wire::DataPlaneType::kGenericNetworkDevice);
  mac_sublayer_support_t resp;
  driver()->QueryMacSublayerSupport(&resp);
  EXPECT_EQ(resp.data_plane.data_plane_type, DATA_PLANE_TYPE_GENERIC_NETWORK_DEVICE);
  EXPECT_EQ(resp.device.mac_implementation_type, MAC_IMPLEMENTATION_TYPE_FULLMAC);
}

TEST_F(WlanifDeviceTest, CheckCallbacks) {
  libsync::Completion signal;
  auto ops = EmptyRustProtoOps();

  ops.deauth_conf = [](void* ctx, const uint8_t peer_sta_address[ETH_ALEN]) {
    EXPECT_NE(ctx, nullptr);
    libsync::Completion* signal = static_cast<libsync::Completion*>(ctx);
    signal->Signal();
  };
  ops.on_scan_result = [](void* ctx, const wlan_fullmac_scan_result_t* result) {
    EXPECT_NE(ctx, nullptr);
    libsync::Completion* signal = static_cast<libsync::Completion*>(ctx);
    signal->Signal();
  };
  ops.on_scan_end = [](void* ctx, const wlan_fullmac_scan_end_t* end) {
    EXPECT_NE(ctx, nullptr);
    libsync::Completion* signal = static_cast<libsync::Completion*>(ctx);
    signal->Signal();
  };
  ops.connect_conf = [](void* ctx, const wlan_fullmac_connect_confirm_t* resp) {
    EXPECT_NE(ctx, nullptr);
    libsync::Completion* signal = static_cast<libsync::Completion*>(ctx);
    signal->Signal();
  };
  ops.roam_conf = [](void* ctx, const wlan_fullmac_roam_confirm_t* resp) {
    EXPECT_NE(ctx, nullptr);
    libsync::Completion* signal = static_cast<libsync::Completion*>(ctx);
    signal->Signal();
  };
  ops.auth_ind = [](void* ctx, const wlan_fullmac_auth_ind_t* ind) {
    EXPECT_NE(ctx, nullptr);
    libsync::Completion* signal = static_cast<libsync::Completion*>(ctx);
    signal->Signal();
  };
  ops.deauth_ind = [](void* ctx, const wlan_fullmac_deauth_indication_t* ind) {
    EXPECT_NE(ctx, nullptr);
    libsync::Completion* signal = static_cast<libsync::Completion*>(ctx);
    signal->Signal();
  };
  ops.assoc_ind = [](void* ctx, const wlan_fullmac_assoc_ind_t* ind) {
    EXPECT_NE(ctx, nullptr);
    libsync::Completion* signal = static_cast<libsync::Completion*>(ctx);
    signal->Signal();
  };
  ops.disassoc_ind = [](void* ctx, const wlan_fullmac_disassoc_indication_t* ind) {
    EXPECT_NE(ctx, nullptr);
    libsync::Completion* signal = static_cast<libsync::Completion*>(ctx);
    signal->Signal();
  };

  EXPECT_EQ(ZX_OK, StartWlanifDevice(&ops, &signal));
  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendDeauthConf);
  zx_status_t status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();

  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendScanResult);
  status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();

  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendScanEnd);
  status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();

  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendConnectConf);
  status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();

  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendRoamConf);
  status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();

  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendAuthInd);
  status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();

  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendDeauthInd);
  status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();

  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendAssocInd);
  status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();

  fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendDisassocInd);
  status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();
}
// TODO(https://fxbug.dev/42072483) Add unit tests for other functions in wlanif::Device
}  // namespace
