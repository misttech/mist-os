// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/connectivity/wlan/drivers/wlanif/device.h"

#include <fuchsia/wlan/ieee80211/cpp/fidl.h>
#include <fuchsia/wlan/mlme/cpp/fidl.h>
#include <fuchsia/wlan/mlme/cpp/fidl_test_base.h>
#include <fuchsia/wlan/sme/cpp/fidl.h>
#include <fuchsia/wlan/sme/cpp/fidl_test_base.h>
#include <lib/async/cpp/task.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/fidl/cpp/decoder.h>
#include <lib/fidl/cpp/message.h>
#include <lib/sync/cpp/completion.h>
#include <lib/sys/cpp/testing/component_context_provider.h>
#include <zircon/errors.h>
#include <zircon/system/ulib/async-default/include/lib/async/default.h>

#include <memory>

#include <zxtest/zxtest.h>

#include "fidl/fuchsia.wlan.fullmac/cpp/wire_types.h"
#include "fuchsia/wlan/fullmac/c/banjo.h"
#include "src/connectivity/wlan/drivers/wlanif/test/test_bss.h"
#include "src/connectivity/wlan/lib/mlme/fullmac/c-binding/bindings.h"
#include "src/devices/testing/mock-ddk/mock-device.h"

namespace {

constexpr uint8_t kPeerStaAddress[6] = {0xba, 0xbe, 0xfa, 0xce, 0x00, 0x00};

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
      .deauth_conf = [](void* ctx, const uint8_t peer_sta_address[6]) {},
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

struct WlanifDeviceTest : public ::zxtest::Test,
                          public fdf::WireServer<fuchsia_wlan_fullmac::WlanFullmacImpl> {
  void SetUp() override {
    auto endpoints_fullmac_impl = fdf::CreateEndpoints<fuchsia_wlan_fullmac::WlanFullmacImpl>();
    ASSERT_FALSE(endpoints_fullmac_impl.is_error());

    device_ = new wlanif::Device(parent_.get(), std::move(endpoints_fullmac_impl->client));

    auto server_dispatcher = fdf_testing::DriverRuntime::GetInstance()->StartBackgroundDispatcher();
    fdf::BindServer(server_dispatcher->get(), std::move(endpoints_fullmac_impl->server), this);
  }

  void TearDown() override { mock_ddk::ReleaseFlaggedDevices(device_->zxdev()); }

  // Calls device_->Start() and throws away the returned out_sme_channel.
  // Must be called at the start of any tests that check that wlanif::Device calls the ops in
  // rust_wlan_fullmac_ifc_protocol_ops_copy_t.
  void StartWlanifDevice(const rust_wlan_fullmac_ifc_protocol_ops_copy_t* ops, void* ctx) {
    rust_wlan_fullmac_ifc_protocol_copy_t proto{
        .ops = ops,
        .ctx = ctx,
    };

    zx::channel out_sme_channel;
    ASSERT_OK(device_->Start(&proto, &out_sme_channel));
  }

  // WlanFullmacImpl implementations, dispatching FIDL requests from wlanif driver.
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
  void Query(fdf::Arena& arena, QueryCompleter::Sync& completer) override {}
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
                  SetKeysReqCompleter::Sync& completer) override {}
  void DelKeysReq(DelKeysReqRequestView request, fdf::Arena& arena,
                  DelKeysReqCompleter::Sync& completer) override {}
  void EapolTx(EapolTxRequestView request, fdf::Arena& arena,
               EapolTxCompleter::Sync& completer) override {}
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

  std::shared_ptr<MockDevice> parent_ = MockDevice::FakeRootParent();

  // Client to fire WlanFullmacImplIfc FIDL requests to wlanif driver.
  fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImplIfc> client_;

  // If the device binds successfully, the parent calls release on this pointer which will delete it
  // so don't delete it or manage it.
  wlanif::Device* device_;

  fuchsia_wlan_common::wire::DataPlaneType data_plane_type_ =
      fuchsia_wlan_common::wire::DataPlaneType::kGenericNetworkDevice;
  bool expected_online_ = false;
  bool link_state_changed_called_ = false;
};

TEST_F(WlanifDeviceTest, EthDataPlaneNotSupported) {
  data_plane_type_ = fuchsia_wlan_common::wire::DataPlaneType::kEthernetDevice;
  ASSERT_EQ(device_->Bind(), ZX_ERR_NOT_SUPPORTED);
  device_->AddDevice();
  device_->DdkAsyncRemove();
}

TEST_F(WlanifDeviceTest, OnLinkStateChangedOnlyCalledWhenStateChanges) {
  device_->AddDevice();
  device_->DdkAsyncRemove();

  expected_online_ = true;
  device_->OnLinkStateChanged(true);
  EXPECT_TRUE(link_state_changed_called_);

  link_state_changed_called_ = false;
  device_->OnLinkStateChanged(true);
  EXPECT_FALSE(link_state_changed_called_);

  link_state_changed_called_ = false;
  expected_online_ = false;
  device_->OnLinkStateChanged(false);
  EXPECT_TRUE(link_state_changed_called_);

  link_state_changed_called_ = false;
  device_->OnLinkStateChanged(false);
  EXPECT_FALSE(link_state_changed_called_);

  link_state_changed_called_ = false;
  expected_online_ = true;
  device_->OnLinkStateChanged(true);
  EXPECT_TRUE(link_state_changed_called_);
}

TEST_F(WlanifDeviceTest, CheckScanReq) {
  device_->AddDevice();
  device_->DdkAsyncRemove();
  const uint8_t chan_list[1] = {0x9};
  const wlan_fullmac_impl_start_scan_request_t req = {.scan_type = WLAN_SCAN_TYPE_ACTIVE,
                                                      .channels_list = chan_list,
                                                      .channels_count = 1,
                                                      .ssids_count = 0,
                                                      .min_channel_time = 10,
                                                      .max_channel_time = 100};
  device_->StartScan(&req);
}

TEST_F(WlanifDeviceTest, CheckConnReq) {
  device_->AddDevice();
  device_->DdkAsyncRemove();
  const wlan_fullmac_impl_connect_request_t req = wlan_fullmac_test::CreateConnectReq();
  device_->Connect(&req);
}

TEST_F(WlanifDeviceTest, CheckReconnReq) {
  device_->AddDevice();
  device_->DdkAsyncRemove();
  wlan_fullmac_impl_reconnect_request_t req;
  memcpy(req.peer_sta_address, kPeerStaAddress, sizeof(kPeerStaAddress));
  device_->Reconnect(&req);
}

TEST_F(WlanifDeviceTest, CheckDeauthReq) {
  device_->AddDevice();
  device_->DdkAsyncRemove();
  wlan_fullmac_impl_deauth_request_t req = {.reason_code = REASON_CODE_AP_INITIATED};
  memcpy(req.peer_sta_address, kPeerStaAddress, sizeof(kPeerStaAddress));
  device_->Deauthenticate(&req);
}

TEST_F(WlanifDeviceTest, CheckAuthResp) {
  device_->AddDevice();
  device_->DdkAsyncRemove();
  wlan_fullmac_impl_auth_resp_request_t resp = {.result_code = WLAN_AUTH_RESULT_SUCCESS};
  memcpy(resp.peer_sta_address, kPeerStaAddress, sizeof(kPeerStaAddress));
  device_->AuthenticateResp(&resp);
}

TEST_F(WlanifDeviceTest, CheckAssocResp) {
  device_->AddDevice();
  device_->DdkAsyncRemove();
  wlan_fullmac_impl_assoc_resp_request resp = {.result_code = WLAN_ASSOC_RESULT_SUCCESS,
                                               .association_id = 42};
  memcpy(resp.peer_sta_address, kPeerStaAddress, sizeof(kPeerStaAddress));
  device_->AssociateResp(&resp);
}

TEST_F(WlanifDeviceTest, CheckDisassoc) {
  device_->AddDevice();
  device_->DdkAsyncRemove();
  wlan_fullmac_impl_disassoc_request_t req = {.reason_code = REASON_CODE_LEAVING_NETWORK_DISASSOC};
  memcpy(req.peer_sta_address, kPeerStaAddress, sizeof(kPeerStaAddress));
  device_->Disassociate(&req);
}

TEST_F(WlanifDeviceTest, CheckStartBss) {
  device_->AddDevice();
  device_->DdkAsyncRemove();
  wlan_fullmac_impl_start_bss_request_t req = {
      .bss_type = BSS_TYPE_INFRASTRUCTURE, .beacon_period = 100, .dtim_period = 100, .channel = 1};
  memcpy(req.ssid.data, kPeerStaAddress, sizeof(kPeerStaAddress));
  req.ssid.len = sizeof(kPeerStaAddress);
  device_->StartBss(&req);
}

TEST_F(WlanifDeviceTest, CheckStopBss) {
  device_->AddDevice();
  device_->DdkAsyncRemove();
  wlan_fullmac_impl_stop_bss_request_t req;
  memcpy(req.ssid.data, kPeerStaAddress, sizeof(kPeerStaAddress));
  req.ssid.len = sizeof(kPeerStaAddress);
  device_->StopBss(&req);
}

TEST_F(WlanifDeviceTest, CheckReset) {
  device_->AddDevice();
  device_->DdkAsyncRemove();
  wlan_fullmac_impl_reset_request_t req = {.set_default_mib = true};
  memcpy(req.sta_address, kPeerStaAddress, sizeof(kPeerStaAddress));
  device_->Reset(&req);
}
// Call DeauthConf on WlanFullmacImplIfc and verify that it calls deauth_conf on the given
// protocol ops with the correct peer_sta_address.
TEST_F(WlanifDeviceTest, CheckDeauthConf) {
  device_->AddDevice();
  device_->DdkAsyncRemove();

  auto ops = EmptyRustProtoOps();
  bool called = false;

  ops.deauth_conf = [](void* ctx, const uint8_t peer_sta_address[6]) {
    for (size_t i = 0; i < 6; i++) {
      EXPECT_EQ(kPeerStaAddress[i], peer_sta_address[i]);
    }

    EXPECT_NOT_NULL(ctx);
    bool* called = static_cast<bool*>(ctx);
    *called = true;
  };

  StartWlanifDevice(&ops, &called);

  fdf::Arena arena = fdf::Arena::Create(0, 0).value();

  fidl::Array<uint8_t, 6> peer_sta_address;
  memcpy(peer_sta_address.data(), kPeerStaAddress, 6);

  auto req = fuchsia_wlan_fullmac::wire::WlanFullmacImplIfcDeauthConfRequest::Builder(arena)
                 .peer_sta_address(peer_sta_address)
                 .Build();

  EXPECT_OK(client_.buffer(arena)->DeauthConf(req));
  EXPECT_TRUE(called);
}
// TODO(https://fxbug.dev/121450) Add unit tests for other functions in wlanif::Device
}  // namespace
