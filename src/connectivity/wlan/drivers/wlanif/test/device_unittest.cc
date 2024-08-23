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

#include <future>
#include <memory>

#include <gtest/gtest.h>

#include "fidl/fuchsia.wlan.common/cpp/wire_types.h"
#include "fidl/fuchsia.wlan.fullmac/cpp/wire_types.h"
#include "fidl/fuchsia.wlan.internal/cpp/wire_types.h"
#include "src/connectivity/wlan/drivers/wlanif/test/test_bss.h"
#include "src/connectivity/wlan/lib/common/cpp/include/wlan/common/macaddr.h"
#include "src/connectivity/wlan/lib/mlme/fullmac/c-binding/testing/bindings_stubs.h"

constexpr zx::duration kWaitForCallbackDuration = zx::msec(1000);
namespace {

constexpr uint8_t kPeerStaAddress[ETH_ALEN] = {0xba, 0xbe, 0xfa, 0xce, 0x00, 0x00};
constexpr uint8_t kSelectedBssid[ETH_ALEN] = {0x1a, 0x2b, 0x3a, 0x4d, 0x5e, 0x6f};

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

class FakeFullmacParent : public fidl::WireServer<fuchsia_wlan_fullmac::WlanFullmacImpl> {
 public:
  void ServiceConnectHandler(fidl::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> server_end) {
    fidl::BindServer(fdf::Dispatcher::GetCurrent()->async_dispatcher(), std::move(server_end),
                     this);
  }
  FakeFullmacParent() {
    memcpy(peer_sta_addr_, kPeerStaAddress, ETH_ALEN);
    memcpy(selected_bssid_, kSelectedBssid, ETH_ALEN);
  }
  void Start(StartRequestView request, StartCompleter::Sync& completer) override {
    client_ =
        fidl::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImplIfc>(std::move(request->ifc));

    // Create and send a mock channel up here since the sme_channel field is required in the reply
    // of Start().
    auto [local, remote] = make_channel();
    completer.ReplySuccess(std::move(local));
  }
  void Stop(StopCompleter::Sync& completer) override {}
  void Query(QueryCompleter::Sync& completer) override {
    fuchsia_wlan_fullmac::wire::WlanFullmacQueryInfo info = {};
    info.role = fuchsia_wlan_common::wire::WlanMacRole::kClient;
    info.band_cap_count = 0;
    completer.ReplySuccess(info);
  }
  void QueryMacSublayerSupport(QueryMacSublayerSupportCompleter::Sync& completer) override {
    fuchsia_wlan_common::wire::MacSublayerSupport mac_sublayer_support;
    mac_sublayer_support.data_plane.data_plane_type = data_plane_type_;
    mac_sublayer_support.device.mac_implementation_type =
        fuchsia_wlan_common::wire::MacImplementationType::kFullmac;
    completer.ReplySuccess(mac_sublayer_support);
  }
  void QuerySecuritySupport(QuerySecuritySupportCompleter::Sync& completer) override {}
  void QuerySpectrumManagementSupport(
      QuerySpectrumManagementSupportCompleter::Sync& completer) override {}
  void StartScan(StartScanRequestView request, StartScanCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_scan_type(), true);
    EXPECT_EQ(request->has_channels(), true);
    EXPECT_EQ(request->has_min_channel_time(), true);
    EXPECT_EQ(request->has_max_channel_time(), true);
    completer.Reply();
  }
  void Connect(ConnectRequestView request, ConnectCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_selected_bss(), true);
    EXPECT_EQ(request->has_auth_type(), true);
    EXPECT_EQ(request->has_connect_failure_timeout(), true);
    EXPECT_EQ(request->has_security_ie(), true);
    completer.Reply();
  }
  void Reconnect(ReconnectRequestView request, ReconnectCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_peer_sta_address(), true);
    completer.Reply();
  }
  void AuthResp(AuthRespRequestView request, AuthRespCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_peer_sta_address(), true);
    EXPECT_EQ(request->has_result_code(), true);
    completer.Reply();
  }
  void Deauth(DeauthRequestView request, DeauthCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_peer_sta_address(), true);
    EXPECT_EQ(request->has_reason_code(), true);
    completer.Reply();
  }
  void AssocResp(AssocRespRequestView request, AssocRespCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_peer_sta_address(), true);
    EXPECT_EQ(request->has_result_code(), true);
    EXPECT_EQ(request->has_association_id(), true);
    completer.Reply();
  }
  void Disassoc(DisassocRequestView request, DisassocCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_peer_sta_address(), true);
    EXPECT_EQ(request->has_reason_code(), true);
    completer.Reply();
  }
  void Reset(ResetRequestView request, ResetCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_sta_address(), true);
    EXPECT_EQ(request->has_set_default_mib(), true);
    completer.Reply();
  }
  void StartBss(StartBssRequestView request, StartBssCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_beacon_period(), true);
    EXPECT_EQ(request->has_bss_type(), true);
    EXPECT_EQ(request->has_channel(), true);
    EXPECT_EQ(request->has_dtim_period(), true);
    EXPECT_EQ(request->has_ssid(), true);
    completer.Reply();
  }
  void StopBss(StopBssRequestView request, StopBssCompleter::Sync& completer) override {
    EXPECT_EQ(request->has_ssid(), true);
    completer.Reply();
  }
  void SetKeysReq(SetKeysReqRequestView request, SetKeysReqCompleter::Sync& completer) override {
    EXPECT_GE(request->req.num_keys, 1ul);
    fuchsia_wlan_fullmac::wire::WlanFullmacSetKeysResp resp = {};
    resp.num_keys = request->req.num_keys;
    completer.Reply(resp);
  }
  void DelKeysReq(DelKeysReqRequestView request, DelKeysReqCompleter::Sync& completer) override {
    EXPECT_GE(request->req.num_keys, 1ul);
    del_key_request_received_ = true;
    completer.Reply();
  }
  void EapolTx(EapolTxRequestView request, EapolTxCompleter::Sync& completer) override {
    EXPECT_GE(request->data().count(), 1ul);
    eapol_tx_request_received_ = true;
    completer.Reply();
  }
  void GetIfaceCounterStats(GetIfaceCounterStatsCompleter::Sync& completer) override {}
  void GetIfaceHistogramStats(GetIfaceHistogramStatsCompleter::Sync& completer) override {}
  void SetMulticastPromisc(SetMulticastPromiscRequestView request,
                           SetMulticastPromiscCompleter::Sync& completer) override {}
  void SaeHandshakeResp(SaeHandshakeRespRequestView request,
                        SaeHandshakeRespCompleter::Sync& completer) override {}
  void SaeFrameTx(SaeFrameTxRequestView request, SaeFrameTxCompleter::Sync& completer) override {}
  void WmmStatusReq(WmmStatusReqCompleter::Sync& completer) override {}
  void OnLinkStateChanged(OnLinkStateChangedRequestView request,
                          OnLinkStateChangedCompleter::Sync& completer) override {
    EXPECT_EQ(request->online, expected_online_);
    link_state_changed_called_ = true;
    completer.Reply();
  }

  auto SendDeauthConf() {
    fidl::Array<uint8_t, ETH_ALEN> peer_sta_address;
    memcpy(peer_sta_address.data(), peer_sta_addr_, ETH_ALEN);

    auto req = fuchsia_wlan_fullmac::wire::WlanFullmacImplIfcDeauthConfRequest::Builder(arena_)
                   .peer_sta_address(peer_sta_address)
                   .Build();
    return client_.buffer(arena_)->DeauthConf(req);
  }

  auto SendScanResult() {
    fuchsia_wlan_fullmac::wire::WlanFullmacScanResult scan_result = {};
    scan_result.bss.bss_type = fuchsia_wlan_common::wire::BssType::kInfrastructure;
    scan_result.bss.channel.primary = 9;
    scan_result.bss.channel.cbw = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw20;

    return client_.buffer(arena_)->OnScanResult(scan_result);
  }

  auto SendScanEnd() {
    fuchsia_wlan_fullmac::wire::WlanFullmacScanEnd scan_end = {};
    scan_end.code = fuchsia_wlan_fullmac::WlanScanResult::kSuccess;
    return client_.buffer(arena_)->OnScanEnd(scan_end);
  }

  auto SendConnectConf() {
    fuchsia_wlan_fullmac::wire::WlanFullmacConnectConfirm conf = {};
    conf.result_code = fuchsia_wlan_ieee80211::wire::StatusCode::kSuccess;
    return client_.buffer(arena_)->ConnectConf(conf);
  }

  auto SendRoamStartInd() {
    fidl::Array<uint8_t, ETH_ALEN> selected_bssid;
    memcpy(selected_bssid.data(), selected_bssid_, ETH_ALEN);
    // Provide a very minimal BSS Description.
    fuchsia_wlan_internal::wire::BssDescription selected_bss{
        .bss_type = fuchsia_wlan_common::wire::BssType::kInfrastructure,
        .beacon_period = 100,
        .channel = fuchsia_wlan_common::wire::WlanChannel{
            .primary = 1,
            .cbw = fuchsia_wlan_common::wire::ChannelBandwidth::kCbw20,
        }};
    memcpy(selected_bss.bssid.data(), selected_bssid_, ETH_ALEN);

    auto req = fuchsia_wlan_fullmac::wire::WlanFullmacImplIfcRoamStartIndRequest::Builder(arena_)
                   .selected_bssid(selected_bssid)
                   .selected_bss(selected_bss)
                   .original_association_maintained(false)
                   .Build();

    return client_.buffer(arena_)->RoamStartInd(req);
  }

  auto SendRoamStartIndWithMissingSelectedBss() {
    fidl::Array<uint8_t, ETH_ALEN> selected_bssid;
    memcpy(selected_bssid.data(), selected_bssid_, ETH_ALEN);
    auto req = fuchsia_wlan_fullmac::wire::WlanFullmacImplIfcRoamStartIndRequest::Builder(arena_)
                   .selected_bssid(selected_bssid)
                   // Note: selected_bss is not included here, to check that MLME will not crash
                   // when it is missing.
                   .original_association_maintained(false)
                   .Build();

    return client_.buffer(arena_)->RoamStartInd(req);
  }

  auto SendRoamResultInd() {
    fidl::Array<uint8_t, ETH_ALEN> selected_bssid;
    memcpy(selected_bssid.data(), selected_bssid_, ETH_ALEN);
    auto req = fuchsia_wlan_fullmac::wire::WlanFullmacImplIfcRoamResultIndRequest::Builder(arena_)
                   .status_code(fuchsia_wlan_ieee80211::wire::StatusCode::kSuccess)
                   .selected_bssid(selected_bssid)
                   .original_association_maintained(false)
                   .target_bss_authenticated(true)
                   .Build();
    return client_.buffer(arena_)->RoamResultInd(req);
  }

  auto SendAuthInd() {
    fuchsia_wlan_fullmac::wire::WlanFullmacAuthInd ind = {};
    ind.auth_type = fuchsia_wlan_fullmac::wire::WlanAuthType::kOpenSystem;
    return client_.buffer(arena_)->AuthInd(ind);
  }

  auto SendDeauthInd() {
    fuchsia_wlan_fullmac::wire::WlanFullmacDeauthIndication ind = {};
    ind.reason_code = fuchsia_wlan_ieee80211::wire::ReasonCode::kUnspecifiedReason;
    return client_.buffer(arena_)->DeauthInd(ind);
  }

  auto SendAssocInd() {
    fuchsia_wlan_fullmac::wire::WlanFullmacAssocInd ind = {};
    return client_.buffer(arena_)->AssocInd(ind);
  }

  auto SendDisassocInd() {
    fuchsia_wlan_fullmac::wire::WlanFullmacDisassocIndication ind = {};
    return client_.buffer(arena_)->DisassocInd(ind);
  }

  auto SendStartConf() {
    fuchsia_wlan_fullmac::wire::WlanFullmacStartConfirm conf = {};
    return client_.buffer(arena_)->StartConf(conf);
  }

  auto SendStopConf() {
    fuchsia_wlan_fullmac::wire::WlanFullmacStopConfirm conf = {};
    return client_.buffer(arena_)->StopConf(conf);
  }

  auto SendEapolConf() {
    fuchsia_wlan_fullmac::wire::WlanFullmacEapolConfirm conf = {};
    return client_.buffer(arena_)->EapolConf(conf);
  }

  auto SendOnChannelSwitch() {
    fuchsia_wlan_fullmac::wire::WlanFullmacChannelSwitchInfo info = {};
    return client_.buffer(arena_)->OnChannelSwitch(info);
  }

  auto SendSignalReport() {
    fuchsia_wlan_fullmac::wire::WlanFullmacSignalReportIndication ind = {};
    return client_.buffer(arena_)->SignalReport(ind);
  }

  auto SendEapolInd() {
    fuchsia_wlan_fullmac::wire::WlanFullmacEapolIndication ind = {};
    return client_.buffer(arena_)->EapolInd(ind);
  }

  auto SendOnPmkAvailable() {
    fuchsia_wlan_fullmac::wire::WlanFullmacPmkInfo info = {};
    return client_.buffer(arena_)->OnPmkAvailable(info);
  }

  auto SendSaeHandshakeInd() {
    fuchsia_wlan_fullmac::wire::WlanFullmacSaeHandshakeInd ind = {};
    return client_.buffer(arena_)->SaeHandshakeInd(ind);
  }

  auto SendSaeFrameRx() {
    fuchsia_wlan_fullmac::wire::WlanFullmacSaeFrame frame = {};
    return client_.buffer(arena_)->SaeFrameRx(frame);
  }

  auto SendOnWmmStatusResp() {
    fuchsia_wlan_common::wire::WlanWmmParameters params = {};
    return client_.buffer(arena_)->OnWmmStatusResp(ZX_OK, params);
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
  fidl::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImplIfc> client_;

  fuchsia_wlan_common::wire::DataPlaneType data_plane_type_ =
      fuchsia_wlan_common::wire::DataPlaneType::kGenericNetworkDevice;
  bool expected_online_ = false;
  bool link_state_changed_called_ = false;
  bool del_key_request_received_ = false;
  bool eapol_tx_request_received_ = false;

 private:
  fdf::Arena arena_ = fdf::Arena::Create(0, 0).value();
  uint8_t peer_sta_addr_[ETH_ALEN];
  uint8_t selected_bssid_[ETH_ALEN];
};

using fdf_testing::TestEnvironment;

class WlanifDeviceTestNoStopOnTeardown : public ::testing::Test {
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
        [this](fidl::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> server_end) {
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
    test_environment_.reset();
    node_server_.reset();
    runtime_.ShutdownAllDispatchers(fdf::Dispatcher::GetCurrent()->get());
    ASSERT_EQ(ZX_OK, driver_.Stop().status_value());
  }

  void DriverPrepareStop() {
    zx::result prepare_stop_result = runtime_.RunToCompletion(driver_.PrepareStop());
    EXPECT_EQ(ZX_OK, prepare_stop_result.status_value());
  }

  zx_status_t StartWlanifDevice(const rust_wlan_fullmac_ifc_protocol_copy_t proto) {
    auto endpoints = fidl::CreateEndpoints<fuchsia_wlan_fullmac::WlanFullmacImplIfc>();
    ZX_ASSERT(endpoints.is_ok());

    zx_handle_t handle = endpoints->server.TakeHandle().release();
    zx_status_t status = driver_->StartFullmacIfcServer(&proto, handle);
    EXPECT_EQ(status, ZX_OK);

    fake_wlanfullmac_parent_.SyncCall([&](FakeFullmacParent* fake_wlanfullmac_parent) {
      fake_wlanfullmac_parent->client_ =
          fidl::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImplIfc>(
              std::move(endpoints->client));
    });
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

  bindings_stubs::FullmacMlmeBindingsStubs bindings_stubs_;
};

TEST_F(WlanifDeviceTestNoStopOnTeardown, CheckRustProtoOpsCannotBeCalledAfterMlmeStopped) {
  auto ops = EmptyRustProtoOps();

  libsync::Completion complete_stop_fullmac_mlme;
  libsync::Completion stop_fullmac_mlme_called;
  bindings_stubs_.stop_fullmac_mlme_stub = [&](wlan_fullmac_mlme_handle_t*) {
    // pause in the middle of this function to be able to check that rust proto ops are not called
    // and WlanFullmacImplIfc calls fail.
    stop_fullmac_mlme_called.Signal();
    complete_stop_fullmac_mlme.Wait();
  };

  ops.deauth_conf = [](void* ctx, const uint8_t peer_sta_address[ETH_ALEN]) { ADD_FAILURE(); };
  ops.on_scan_result = [](void* ctx, const wlan_fullmac_scan_result_t* result) { ADD_FAILURE(); };
  ops.on_scan_end = [](void* ctx, const wlan_fullmac_scan_end_t* end) { ADD_FAILURE(); };
  ops.connect_conf = [](void* ctx, const wlan_fullmac_connect_confirm_t* resp) { ADD_FAILURE(); };
  ops.roam_start_ind = [](void* ctx, const unsigned char*, const bss_description*, bool) {
    ADD_FAILURE();
  };
  ops.roam_result_ind = [](void* ctx, const unsigned char*, uint16_t, bool, bool, uint16_t,
                           const unsigned char*, uint64_t) -> void { ADD_FAILURE(); };
  ops.auth_ind = [](void* ctx, const wlan_fullmac_auth_ind_t* ind) { ADD_FAILURE(); };
  ops.deauth_ind = [](void* ctx, const wlan_fullmac_deauth_indication_t* ind) { ADD_FAILURE(); };
  ops.assoc_ind = [](void* ctx, const wlan_fullmac_assoc_ind_t* ind) { ADD_FAILURE(); };
  ops.disassoc_ind = [](void* ctx, const wlan_fullmac_disassoc_indication_t* ind) {
    ADD_FAILURE();
  };
  ops.start_conf = [](void* ctx, const wlan_fullmac_start_confirm_t* resp) { ADD_FAILURE(); };
  ops.stop_conf = [](void* ctx, const wlan_fullmac_stop_confirm_t* resp) { ADD_FAILURE(); };
  ops.eapol_conf = [](void* ctx, const wlan_fullmac_eapol_confirm_t* resp) { ADD_FAILURE(); };
  ops.on_channel_switch = [](void* ctx, const wlan_fullmac_channel_switch_info_t* resp) {
    ADD_FAILURE();
  };
  ops.signal_report = [](void* ctx, const wlan_fullmac_signal_report_indication_t* ind) {
    ADD_FAILURE();
  };
  ops.eapol_ind = [](void* ctx, const wlan_fullmac_eapol_indication_t* ind) { ADD_FAILURE(); };
  ops.on_pmk_available = [](void* ctx, const wlan_fullmac_pmk_info_t* info) { ADD_FAILURE(); };
  ops.sae_handshake_ind = [](void* ctx, const wlan_fullmac_sae_handshake_ind_t* ind) {
    ADD_FAILURE();
  };
  ops.sae_frame_rx = [](void* ctx, const wlan_fullmac_sae_frame_t* frame) { ADD_FAILURE(); };
  ops.on_wmm_status_resp = [](void* ctx, zx_status_t status,
                              const wlan_wmm_parameters_t* wmm_params) { ADD_FAILURE(); };

  EXPECT_EQ(ZX_OK, StartWlanifDevice({&ops, nullptr}));

  [[maybe_unused]] auto f = std::async(std::launch::async, [&]() {
    stop_fullmac_mlme_called.Wait();
    // now we know that we're in stop_fullmac_mlme

    // ensure that WlanFullmacImplIfc calls don't result in FFI calls
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendDeauthConf).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendScanResult).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendScanEnd).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendConnectConf).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendRoamStartInd).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_
                    .SyncCall(&FakeFullmacParent::SendRoamStartIndWithMissingSelectedBss)
                    .ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendRoamResultInd).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendAuthInd).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendDeauthInd).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendAssocInd).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendDisassocInd).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendStartConf).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendStopConf).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendEapolConf).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendOnChannelSwitch).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendSignalReport).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendEapolInd).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendOnPmkAvailable).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendSaeHandshakeInd).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendSaeFrameRx).ok());
    EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendOnWmmStatusResp).ok());

    // let driver complete operation
    complete_stop_fullmac_mlme.Signal();
  });

  DriverPrepareStop();
}

class WlanifDeviceTest : public WlanifDeviceTestNoStopOnTeardown {
 public:
  void TearDown() override {
    DriverPrepareStop();
    WlanifDeviceTestNoStopOnTeardown::TearDown();
  }
};

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

  ops.roam_start_ind = [](void* ctx, const unsigned char*, const bss_description*, bool) {
    EXPECT_NE(ctx, nullptr);
    libsync::Completion* signal = static_cast<libsync::Completion*>(ctx);
    signal->Signal();
  };

  ops.roam_result_ind = [](void* ctx, const unsigned char*, uint16_t, bool, bool, uint16_t,
                           const unsigned char*, uint64_t) {
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

  EXPECT_EQ(ZX_OK, StartWlanifDevice({&ops, &signal}));
  EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendDeauthConf).ok());

  zx_status_t status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();

  EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendScanResult).ok());
  status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();

  EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendScanEnd).ok());
  status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();

  EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendConnectConf).ok());
  status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();

  EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendRoamStartInd).ok());
  status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();

  EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendRoamResultInd).ok());
  status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();

  EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendAuthInd).ok());
  status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();

  EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendDeauthInd).ok());
  status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();

  EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendAssocInd).ok());
  status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();

  EXPECT_TRUE(fake_wlanfullmac_parent_.SyncCall(&FakeFullmacParent::SendDisassocInd).ok());
  status = signal.Wait(kWaitForCallbackDuration);
  EXPECT_EQ(status, ZX_OK);
  signal.Reset();
}

// TODO(https://fxbug.dev/42072483) Add unit tests for other functions in wlanif::Device
}  // namespace
