// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/wlan/common/c/banjo.h>
#include <fuchsia/wlan/ieee80211/cpp/fidl.h>
#include <fuchsia/wlan/internal/c/banjo.h>
#include <lib/inspect/cpp/hierarchy.h>
#include <lib/inspect/cpp/inspect.h>
#include <zircon/errors.h>

#include <wifi/wifi-config.h>
#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/testing/lib/sim-device/device.h"
#include "src/connectivity/wlan/drivers/testing/lib/sim-env/sim-env.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/cfg80211.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/fwil.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/test/device_inspect_test_utils.h"
#include "src/connectivity/wlan/lib/common/cpp/include/wlan/common/macaddr.h"

namespace wlan::brcmfmac {

namespace {

constexpr zx::duration kSimulatedClockDuration = zx::sec(10);

}  // namespace

namespace wlan_ieee80211 = wlan_ieee80211;

constexpr uint16_t kDefaultCh = 149;
constexpr wlan_common::WlanChannel kDefaultChannel = {
    .primary = kDefaultCh, .cbw = wlan_common::ChannelBandwidth::kCbw20, .secondary80 = 0};
const common::MacAddr kFakeMac({0xde, 0xad, 0xbe, 0xef, 0x00, 0x02});
constexpr wlan_ieee80211::CSsid kDefaultSsid = {.len = 6, .data = {.data_ = "Sim_AP"}};

class CreateSoftAPTest;

class SoftApInterface : public SimInterface {
 public:
  void AuthInd(AuthIndRequestView request, fdf::Arena& arena,
               AuthIndCompleter::Sync& completer) override;
  void DeauthInd(DeauthIndRequestView request, fdf::Arena& arena,
                 DeauthIndCompleter::Sync& completer) override;
  void DeauthConf(DeauthConfRequestView request, fdf::Arena& arena,
                  DeauthConfCompleter::Sync& completer) override;
  void AssocInd(AssocIndRequestView request, fdf::Arena& arena,
                AssocIndCompleter::Sync& completer) override;
  void DisassocConf(DisassocConfRequestView request, fdf::Arena& arena,
                    DisassocConfCompleter::Sync& completer) override;
  void DisassocInd(DisassocIndRequestView request, fdf::Arena& arena,
                   DisassocIndCompleter::Sync& completer) override;
  void StartConf(StartConfRequestView request, fdf::Arena& arena,
                 StartConfCompleter::Sync& completer) override;
  void StopConf(StopConfRequestView request, fdf::Arena& arena,
                StopConfCompleter::Sync& completer) override;
  void OnChannelSwitch(OnChannelSwitchRequestView request, fdf::Arena& arena,
                       OnChannelSwitchCompleter::Sync& completer) override;

  CreateSoftAPTest* test_;
};

class CreateSoftAPTest : public SimTest {
 public:
  CreateSoftAPTest() = default;
  void Init();
  void CreateInterface();
  void DeleteInterface();
  zx_status_t StartSoftAP();
  zx_status_t StopSoftAP();
  uint32_t DeviceCountByProtocolId(uint32_t proto_id);

  // We track a specific firmware error condition seen in AP start.
  void GetApSetSsidErrInspectCount(uint64_t* out_count);

  void TxAuthReq(simulation::SimAuthType auth_type, common::MacAddr client_mac);
  void TxAssocReq(common::MacAddr client_mac);
  void TxDisassocReq(common::MacAddr client_mac);
  void TxDeauthReq(common::MacAddr client_mac);
  void DeauthClient(common::MacAddr client_mac);
  void VerifyAuth();
  void VerifyAssoc();
  void VerifyNotAssoc();
  void VerifyStartAPConf(wlan_fullmac::WlanStartResult status);
  void VerifyStopAPConf(wlan_fullmac::WlanStopResult status);
  void VerifyNumOfClient(uint16_t expect_client_num);
  void ClearAssocInd();
  void InjectStartAPError();
  void InjectChanspecError();
  void InjectSetSsidError();
  void InjectStopAPError();
  void SetExpectMacForInds(common::MacAddr set_mac);

  void OnAuthInd(const wlan_fullmac::WlanFullmacAuthInd* ind);
  void OnDeauthInd(const wlan_fullmac::WlanFullmacDeauthIndication* ind);
  void OnDeauthConf(const wlan_fullmac::WlanFullmacDeauthConfirm* resp);
  void OnAssocInd(const wlan_fullmac::WlanFullmacAssocInd* ind);
  void OnDisassocConf(const wlan_fullmac::WlanFullmacDisassocConfirm* resp);
  void OnDisassocInd(const wlan_fullmac::WlanFullmacDisassocIndication* ind);
  void OnStartConf(const wlan_fullmac::WlanFullmacStartConfirm* resp);
  void OnStopConf(const wlan_fullmac::WlanFullmacStopConfirm* resp);
  void OnChannelSwitch(const wlan_fullmac::WlanFullmacChannelSwitchInfo* info);
  // Status field in the last received authentication frame.
  wlan_ieee80211::StatusCode auth_resp_status_;

  bool auth_ind_recv_ = false;
  bool assoc_ind_recv_ = false;
  bool deauth_conf_recv_ = false;
  bool deauth_ind_recv_ = false;
  bool disassoc_ind_recv_ = false;
  bool disassoc_conf_recv_ = false;
  bool start_conf_received_ = false;
  bool stop_conf_received_ = false;
  wlan_fullmac::WlanStartResult start_conf_status_;
  wlan_fullmac::WlanStopResult stop_conf_status_;

  // The expect mac address for indications
  common::MacAddr ind_expect_mac_ = kFakeMac;

 protected:
  simulation::WlanTxInfo tx_info_ = {.channel = kDefaultChannel};
  bool sec_enabled_ = false;

 private:
  void Rx(std::shared_ptr<const simulation::SimFrame> frame,
          std::shared_ptr<const simulation::WlanRxInfo> info) override;
  // SME callbacks
  SoftApInterface softap_ifc_;

  uint16_t CreateRsneIe(uint8_t* buffer);
};

void SoftApInterface::AuthInd(AuthIndRequestView request, fdf::Arena& arena,
                              AuthIndCompleter::Sync& completer) {
  test_->OnAuthInd(&request->resp);
  completer.buffer(arena).Reply();
}
void SoftApInterface::DeauthInd(DeauthIndRequestView request, fdf::Arena& arena,
                                DeauthIndCompleter::Sync& completer) {
  test_->OnDeauthInd(&request->ind);
  completer.buffer(arena).Reply();
}
void SoftApInterface::DeauthConf(DeauthConfRequestView request, fdf::Arena& arena,
                                 DeauthConfCompleter::Sync& completer) {
  test_->OnDeauthConf(&request->resp);
  completer.buffer(arena).Reply();
}
void SoftApInterface::AssocInd(AssocIndRequestView request, fdf::Arena& arena,
                               AssocIndCompleter::Sync& completer) {
  test_->OnAssocInd(&request->resp);
  completer.buffer(arena).Reply();
}
void SoftApInterface::DisassocConf(DisassocConfRequestView request, fdf::Arena& arena,
                                   DisassocConfCompleter::Sync& completer) {
  test_->OnDisassocConf(&request->resp);
  completer.buffer(arena).Reply();
}
void SoftApInterface::DisassocInd(DisassocIndRequestView request, fdf::Arena& arena,
                                  DisassocIndCompleter::Sync& completer) {
  test_->OnDisassocInd(&request->ind);
  completer.buffer(arena).Reply();
}
void SoftApInterface::StartConf(StartConfRequestView request, fdf::Arena& arena,
                                StartConfCompleter::Sync& completer) {
  test_->OnStartConf(&request->resp);
  completer.buffer(arena).Reply();
}
void SoftApInterface::StopConf(StopConfRequestView request, fdf::Arena& arena,
                               StopConfCompleter::Sync& completer) {
  test_->OnStopConf(&request->resp);
  completer.buffer(arena).Reply();
}
void SoftApInterface::OnChannelSwitch(OnChannelSwitchRequestView request, fdf::Arena& arena,
                                      OnChannelSwitchCompleter::Sync& completer) {
  test_->OnChannelSwitch(&request->ind);
  completer.buffer(arena).Reply();
}

void CreateSoftAPTest::Rx(std::shared_ptr<const simulation::SimFrame> frame,
                          std::shared_ptr<const simulation::WlanRxInfo> info) {
  ASSERT_EQ(frame->FrameType(), simulation::SimFrame::FRAME_TYPE_MGMT);

  auto mgmt_frame = std::static_pointer_cast<const simulation::SimManagementFrame>(frame);
  if (mgmt_frame->MgmtFrameType() == simulation::SimManagementFrame::FRAME_TYPE_AUTH) {
    auto auth_frame = std::static_pointer_cast<const simulation::SimAuthFrame>(mgmt_frame);
    if (auth_frame->seq_num_ == 2)
      auth_resp_status_ = auth_frame->status_;
  }
}

void CreateSoftAPTest::Init() {
  ASSERT_EQ(SimTest::Init(), ZX_OK);
  softap_ifc_.test_ = this;
}

void CreateSoftAPTest::CreateInterface() {
  zx_status_t status;

  status = SimTest::StartInterface(wlan_common::WlanMacRole::kAp, &softap_ifc_);
  ASSERT_EQ(status, ZX_OK);
}

void CreateSoftAPTest::DeleteInterface() {
  EXPECT_EQ(SimTest::DeleteInterface(&softap_ifc_), ZX_OK);
}

uint32_t CreateSoftAPTest::DeviceCountByProtocolId(uint32_t proto_id) {
  return dev_mgr_->DeviceCountByProtocolId(proto_id);
}

void CreateSoftAPTest::GetApSetSsidErrInspectCount(uint64_t* out_count) {
  ASSERT_NOT_NULL(out_count);
  auto hierarchy = FetchHierarchy(device_->GetInspect()->inspector());
  auto* root = hierarchy.value().GetByPath({"brcmfmac-phy"});
  ASSERT_NOT_NULL(root);
  // Only verify the value of hourly counter here, the relationship between hourly counter and daily
  // counter is verified in device_inspect_test.
  auto* uint_property = root->node().get_property<inspect::UintPropertyValue>("ap_set_ssid_err");
  ASSERT_NOT_NULL(uint_property);
  *out_count = uint_property->value();
}

uint16_t CreateSoftAPTest::CreateRsneIe(uint8_t* buffer) {
  // construct a fake rsne ie in the input buffer
  uint16_t offset = 0;
  uint8_t* ie = buffer;

  ie[offset++] = WLAN_IE_TYPE_RSNE;
  ie[offset++] = 20;  // The length of following content.

  // These two bytes are 16-bit version number.
  ie[offset++] = 1;  // Lower byte
  ie[offset++] = 0;  // Higher byte

  memcpy(&ie[offset], RSN_OUI,
         TLV_OUI_LEN);  // RSN OUI for multicast cipher suite.
  offset += TLV_OUI_LEN;
  ie[offset++] = WPA_CIPHER_TKIP;  // Set multicast cipher suite.

  // These two bytes indicate the length of unicast cipher list, in this case is 1.
  ie[offset++] = 1;  // Lower byte
  ie[offset++] = 0;  // Higher byte

  memcpy(&ie[offset], RSN_OUI,
         TLV_OUI_LEN);  // RSN OUI for unicast cipher suite.
  offset += TLV_OUI_LEN;
  ie[offset++] = WPA_CIPHER_CCMP_128;  // Set unicast cipher suite.

  // These two bytes indicate the length of auth management suite list, in this case is 1.
  ie[offset++] = 1;  // Lower byte
  ie[offset++] = 0;  // Higher byte

  memcpy(&ie[offset], RSN_OUI,
         TLV_OUI_LEN);  // RSN OUI for auth management suite.
  offset += TLV_OUI_LEN;
  ie[offset++] = RSN_AKM_PSK;  // Set auth management suite.

  // These two bytes indicate RSN capabilities, in this case is \x0c\x00.
  ie[offset++] = 12;  // Lower byte
  ie[offset++] = 0;   // Higher byte

  // ASSERT_EQ(offset, (const uint32_t) (ie[TLV_LEN_OFF] + TLV_HDR_LEN));
  return offset;
}

zx_status_t CreateSoftAPTest::StartSoftAP() {
  wlan_fullmac::WlanFullmacStartReq start_req = {
      .ssid = {.len = 6, .data = {.data_ = "Sim_AP"}},
      .bss_type = fuchsia_wlan_common::wire::BssType::kInfrastructure,
      .beacon_period = 100,
      .dtim_period = 100,
      .channel = kDefaultCh,
      .rsne_len = 0,
  };
  // If sec mode is requested, create a dummy RSNE IE (our SoftAP only
  // supports WPA2)
  if (sec_enabled_ == true) {
    start_req.rsne_len = CreateRsneIe(start_req.rsne.data());
  }
  auto result = softap_ifc_.client_.buffer(softap_ifc_.test_arena_)->StartReq(start_req);
  EXPECT_TRUE(result.ok());

  // Retrieve wsec from SIM FW to check if it is set appropriately
  brcmf_simdev* sim = device_->GetSim();
  uint32_t wsec;
  struct brcmf_if* ifp = brcmf_get_ifp(sim->drvr, softap_ifc_.iface_id_);
  zx_status_t status = brcmf_fil_iovar_int_get(ifp, "wsec", &wsec, nullptr);
  EXPECT_EQ(status, ZX_OK);
  if (sec_enabled_ == true)
    EXPECT_NE(wsec, (uint32_t)0);
  else
    EXPECT_EQ(wsec, (uint32_t)0);
  return ZX_OK;
}

void CreateSoftAPTest::InjectStartAPError() {
  brcmf_simdev* sim = device_->GetSim();
  sim->sim_fw->err_inj_.AddErrInjCmd(BRCMF_C_SET_SSID, ZX_ERR_IO, BCME_OK, softap_ifc_.iface_id_);
}

void CreateSoftAPTest::InjectStopAPError() {
  brcmf_simdev* sim = device_->GetSim();
  sim->sim_fw->err_inj_.AddErrInjIovar("bss", ZX_ERR_IO, BCME_OK, softap_ifc_.iface_id_);
}

void CreateSoftAPTest::InjectChanspecError() {
  brcmf_simdev* sim = device_->GetSim();
  sim->sim_fw->err_inj_.AddErrInjIovar("chanspec", ZX_ERR_IO, BCME_BADARG, softap_ifc_.iface_id_);
}

void CreateSoftAPTest::InjectSetSsidError() {
  brcmf_simdev* sim = device_->GetSim();
  sim->sim_fw->err_inj_.AddErrInjCmd(BRCMF_C_SET_SSID, ZX_OK, BCME_ERROR, softap_ifc_.iface_id_);
}

void CreateSoftAPTest::SetExpectMacForInds(common::MacAddr set_mac) { ind_expect_mac_ = set_mac; }

zx_status_t CreateSoftAPTest::StopSoftAP() {
  wlan_fullmac::WlanFullmacStopReq stop_req{
      .ssid = {.len = 6, .data = {.data_ = "Sim_AP"}},
  };
  auto result = softap_ifc_.client_.buffer(softap_ifc_.test_arena_)->StopReq(stop_req);
  EXPECT_TRUE(result.ok());
  return ZX_OK;
}

void CreateSoftAPTest::OnAuthInd(const wlan_fullmac::WlanFullmacAuthInd* ind) {
  ASSERT_EQ(std::memcmp(ind->peer_sta_address.data(), ind_expect_mac_.byte, ETH_ALEN), 0);
  auth_ind_recv_ = true;
}
void CreateSoftAPTest::OnDeauthInd(const wlan_fullmac::WlanFullmacDeauthIndication* ind) {
  ASSERT_EQ(std::memcmp(ind->peer_sta_address.data(), ind_expect_mac_.byte, ETH_ALEN), 0);
  deauth_ind_recv_ = true;
}
void CreateSoftAPTest::OnDeauthConf(const wlan_fullmac::WlanFullmacDeauthConfirm* resp) {
  ASSERT_EQ(std::memcmp(resp->peer_sta_address.data(), ind_expect_mac_.byte, ETH_ALEN), 0);
  deauth_conf_recv_ = true;
}
void CreateSoftAPTest::OnAssocInd(const wlan_fullmac::WlanFullmacAssocInd* ind) {
  ASSERT_EQ(std::memcmp(ind->peer_sta_address.data(), ind_expect_mac_.byte, ETH_ALEN), 0);
  assoc_ind_recv_ = true;
}
void CreateSoftAPTest::OnDisassocConf(const wlan_fullmac::WlanFullmacDisassocConfirm* resp) {
  disassoc_conf_recv_ = true;
}
void CreateSoftAPTest::OnDisassocInd(const wlan_fullmac::WlanFullmacDisassocIndication* ind) {
  ASSERT_EQ(std::memcmp(ind->peer_sta_address.data(), ind_expect_mac_.byte, ETH_ALEN), 0);
  disassoc_ind_recv_ = true;
}

void CreateSoftAPTest::OnStartConf(const wlan_fullmac::WlanFullmacStartConfirm* resp) {
  start_conf_received_ = true;
  start_conf_status_ = resp->result_code;
}

void CreateSoftAPTest::OnStopConf(const wlan_fullmac::WlanFullmacStopConfirm* resp) {
  stop_conf_received_ = true;
  stop_conf_status_ = resp->result_code;
}

void CreateSoftAPTest::OnChannelSwitch(const wlan_fullmac::WlanFullmacChannelSwitchInfo* info) {}

void CreateSoftAPTest::TxAssocReq(common::MacAddr client_mac) {
  // Get the mac address of the SoftAP
  common::MacAddr soft_ap_mac;
  softap_ifc_.GetMacAddr(&soft_ap_mac);
  simulation::SimAssocReqFrame assoc_req_frame(client_mac, soft_ap_mac, kDefaultSsid);
  env_->Tx(assoc_req_frame, tx_info_, this);
}

void CreateSoftAPTest::TxAuthReq(simulation::SimAuthType auth_type, common::MacAddr client_mac) {
  // Get the mac address of the SoftAP
  common::MacAddr soft_ap_mac;
  softap_ifc_.GetMacAddr(&soft_ap_mac);
  simulation::SimAuthFrame auth_req_frame(client_mac, soft_ap_mac, 1, auth_type,
                                          wlan_ieee80211::StatusCode::kSuccess);
  env_->Tx(auth_req_frame, tx_info_, this);
}

void CreateSoftAPTest::TxDisassocReq(common::MacAddr client_mac) {
  // Get the mac address of the SoftAP
  common::MacAddr soft_ap_mac;
  softap_ifc_.GetMacAddr(&soft_ap_mac);
  // Disassociate with the SoftAP
  simulation::SimDisassocReqFrame disassoc_req_frame(
      client_mac, soft_ap_mac, wlan_ieee80211::ReasonCode::kLeavingNetworkDisassoc);
  env_->Tx(disassoc_req_frame, tx_info_, this);
}

void CreateSoftAPTest::TxDeauthReq(common::MacAddr client_mac) {
  // Get the mac address of the SoftAP
  common::MacAddr soft_ap_mac;
  softap_ifc_.GetMacAddr(&soft_ap_mac);
  // Disassociate with the SoftAP
  simulation::SimDeauthFrame deauth_frame(client_mac, soft_ap_mac,
                                          wlan_ieee80211::ReasonCode::kLeavingNetworkDeauth);
  env_->Tx(deauth_frame, tx_info_, this);
}

void CreateSoftAPTest::DeauthClient(common::MacAddr client_mac) {
  wlan_fullmac::WlanFullmacDeauthReq req;

  memcpy(req.peer_sta_address.data(), client_mac.byte, ETH_ALEN);
  req.reason_code = wlan_ieee80211::ReasonCode::kReserved0;

  auto result = softap_ifc_.client_.buffer(softap_ifc_.test_arena_)->DeauthReq(req);
  EXPECT_TRUE(result.ok());
}

void CreateSoftAPTest::VerifyAuth() {
  ASSERT_EQ(auth_ind_recv_, true);
  // When auth is done, the client is already added into client list.
  VerifyNumOfClient(1);
}

void CreateSoftAPTest::VerifyAssoc() {
  // Verify the event indications were received and
  // the number of clients
  ASSERT_EQ(auth_ind_recv_, true);
  ASSERT_EQ(assoc_ind_recv_, true);
  VerifyNumOfClient(1);
}

void CreateSoftAPTest::ClearAssocInd() { assoc_ind_recv_ = false; }

void CreateSoftAPTest::VerifyNumOfClient(uint16_t expect_client_num) {
  brcmf_simdev* sim = device_->GetSim();
  uint16_t num_clients = sim->sim_fw->GetNumClients(softap_ifc_.iface_id_);
  ASSERT_EQ(num_clients, expect_client_num);
}

void CreateSoftAPTest::VerifyNotAssoc() {
  ASSERT_EQ(assoc_ind_recv_, false);
  ASSERT_EQ(auth_ind_recv_, false);
  VerifyNumOfClient(0);
}

void CreateSoftAPTest::VerifyStartAPConf(wlan_fullmac::WlanStartResult status) {
  ASSERT_EQ(start_conf_received_, true);
  ASSERT_EQ(start_conf_status_, status);
}

void CreateSoftAPTest::VerifyStopAPConf(wlan_fullmac::WlanStopResult status) {
  ASSERT_EQ(stop_conf_received_, true);
  ASSERT_EQ(stop_conf_status_, status);
}

TEST_F(CreateSoftAPTest, SetDefault) {
  Init();
  CreateInterface();
  DeleteInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 0u);
}

TEST_F(CreateSoftAPTest, CreateSoftAP) {
  Init();
  CreateInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);
  zx::duration delay = zx::msec(10);
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::StartSoftAP, this), delay);
  delay += kStartAPLinkEventDelay + kApStartedEventDelay + zx::msec(10);
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::StopSoftAP, this), delay);
  env_->Run(kSimulatedClockDuration);
  VerifyStartAPConf(wlan_fullmac::WlanStartResult::kSuccess);
}

TEST_F(CreateSoftAPTest, CreateSoftAPFail) {
  Init();
  CreateInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);
  InjectStartAPError();
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::StartSoftAP, this), zx::msec(50));
  env_->Run(kSimulatedClockDuration);
  VerifyStartAPConf(wlan_fullmac::WlanStartResult::kNotSupported);
}

TEST_F(CreateSoftAPTest, CreateSoftAPFail_ChanSetError) {
  Init();
  CreateInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);
  InjectChanspecError();
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::StartSoftAP, this), zx::msec(50));
  env_->Run(kSimulatedClockDuration);
  VerifyStartAPConf(wlan_fullmac::WlanStartResult::kNotSupported);
}

// SoftAP can encounter this specific SET_SSID firmware error, which we detect and log.
TEST_F(CreateSoftAPTest, CreateSoftAPFail_SetSsidError) {
  Init();
  CreateInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);
  InjectSetSsidError();
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::StartSoftAP, this), zx::msec(50));
  uint64_t count;
  GetApSetSsidErrInspectCount(&count);
  ASSERT_EQ(count, 0u);
  env_->Run(kSimulatedClockDuration);

  VerifyStartAPConf(wlan_fullmac::WlanStartResult::kNotSupported);

  // Verify inspect is updated.
  GetApSetSsidErrInspectCount(&count);
  EXPECT_EQ(count, 1u);
}

// Fail the iovar bss but Stop AP should still succeed
TEST_F(CreateSoftAPTest, BssIovarFail) {
  Init();
  CreateInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);
  InjectStopAPError();
  // Start SoftAP
  StartSoftAP();
  StopSoftAP();
  VerifyStopAPConf(wlan_fullmac::WlanStopResult::kSuccess);
}
// Start SoftAP in secure mode and then restart in open mode.
// Appropriate secure mode is checked in StartSoftAP() after SoftAP
// is started
TEST_F(CreateSoftAPTest, CreateSecureSoftAP) {
  Init();
  CreateInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);
  // Start SoftAP in secure mode
  sec_enabled_ = true;
  StartSoftAP();
  StopSoftAP();
  // Restart SoftAP in open mode
  StartSoftAP();
  StopSoftAP();
}

TEST_F(CreateSoftAPTest, AssociateWithSoftAP) {
  Init();
  CreateInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);
  StartSoftAP();
  env_->ScheduleNotification(
      std::bind(&CreateSoftAPTest::TxAuthReq, this, simulation::AUTH_TYPE_OPEN, kFakeMac),
      zx::msec(5));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::VerifyAuth, this), zx::msec(8));

  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::TxAssocReq, this, kFakeMac),
                             zx::msec(10));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::VerifyAssoc, this), zx::msec(50));
  env_->Run(kSimulatedClockDuration);
}

TEST_F(CreateSoftAPTest, DisassociateThenAssociateWithSoftAP) {
  Init();
  CreateInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);
  StartSoftAP();
  env_->ScheduleNotification(
      std::bind(&CreateSoftAPTest::TxAuthReq, this, simulation::AUTH_TYPE_OPEN, kFakeMac),
      zx::msec(5));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::VerifyAuth, this), zx::msec(8));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::TxAssocReq, this, kFakeMac),
                             zx::msec(10));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::VerifyAssoc, this), zx::msec(50));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::ClearAssocInd, this), zx::msec(75));
  // Assoc a second time
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::TxAssocReq, this, kFakeMac),
                             zx::msec(100));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::VerifyAssoc, this), zx::msec(150));
  env_->Run(kSimulatedClockDuration);
}

TEST_F(CreateSoftAPTest, DisassociateFromSoftAP) {
  Init();
  CreateInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);
  StartSoftAP();
  env_->ScheduleNotification(
      std::bind(&CreateSoftAPTest::TxAuthReq, this, simulation::AUTH_TYPE_OPEN, kFakeMac),
      zx::msec(5));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::VerifyAuth, this), zx::msec(8));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::TxAssocReq, this, kFakeMac),
                             zx::msec(10));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::VerifyAssoc, this), zx::msec(50));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::TxDisassocReq, this, kFakeMac),
                             zx::msec(60));
  env_->Run(kSimulatedClockDuration);
  // Only disassoc ind should be seen.
  EXPECT_EQ(deauth_ind_recv_, false);
  EXPECT_EQ(disassoc_ind_recv_, true);
  VerifyNumOfClient(0);
}

// After a client associates, deauth it from the SoftAP itself.
TEST_F(CreateSoftAPTest, DisassociateClientFromSoftAP) {
  Init();
  CreateInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);
  StartSoftAP();
  env_->ScheduleNotification(
      std::bind(&CreateSoftAPTest::TxAuthReq, this, simulation::AUTH_TYPE_OPEN, kFakeMac),
      zx::msec(5));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::VerifyAuth, this), zx::msec(8));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::TxAssocReq, this, kFakeMac),
                             zx::msec(10));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::VerifyAssoc, this), zx::msec(50));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::DeauthClient, this, kFakeMac),
                             zx::msec(60));
  env_->Run(kSimulatedClockDuration);
  // Should have received disassoc conf.
  EXPECT_EQ(disassoc_conf_recv_, true);
  VerifyNumOfClient(0);
}

TEST_F(CreateSoftAPTest, AssocWithWrongAuth) {
  Init();
  CreateInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);
  StartSoftAP();
  env_->ScheduleNotification(
      std::bind(&CreateSoftAPTest::TxAuthReq, this, simulation::AUTH_TYPE_SHARED_KEY, kFakeMac),
      zx::msec(5));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::TxAssocReq, this, kFakeMac),
                             zx::msec(10));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::VerifyNotAssoc, this), zx::msec(20));
  env_->Run(kSimulatedClockDuration);
  EXPECT_EQ(auth_resp_status_, wlan_ieee80211::StatusCode::kRefusedReasonUnspecified);
}

TEST_F(CreateSoftAPTest, DeauthBeforeAssoc) {
  Init();
  CreateInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);
  StartSoftAP();
  env_->ScheduleNotification(
      std::bind(&CreateSoftAPTest::TxAuthReq, this, simulation::AUTH_TYPE_OPEN, kFakeMac),
      zx::msec(5));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::VerifyAuth, this), zx::msec(10));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::TxDeauthReq, this, kFakeMac),
                             zx::msec(20));
  env_->Run(kSimulatedClockDuration);
  // Only deauth ind shoulb be seen.
  EXPECT_EQ(deauth_ind_recv_, true);
  EXPECT_EQ(disassoc_ind_recv_, false);
  VerifyNumOfClient(0);
}

TEST_F(CreateSoftAPTest, DeauthWhileAssociated) {
  Init();
  CreateInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);
  StartSoftAP();
  env_->ScheduleNotification(
      std::bind(&CreateSoftAPTest::TxAuthReq, this, simulation::AUTH_TYPE_OPEN, kFakeMac),
      zx::msec(5));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::VerifyAuth, this), zx::msec(8));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::TxAssocReq, this, kFakeMac),
                             zx::msec(10));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::VerifyAssoc, this), zx::msec(50));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::TxDeauthReq, this, kFakeMac),
                             zx::msec(60));
  env_->Run(kSimulatedClockDuration);
  // Both indication should be seen.
  EXPECT_EQ(deauth_ind_recv_, true);
  EXPECT_EQ(disassoc_ind_recv_, true);
  VerifyNumOfClient(0);
}

const common::MacAddr kSecondClientMac({0xde, 0xad, 0xbe, 0xef, 0x00, 0x04});

TEST_F(CreateSoftAPTest, DeauthMultiClients) {
  Init();
  CreateInterface();
  EXPECT_EQ(DeviceCountByProtocolId(ZX_PROTOCOL_WLAN_FULLMAC_IMPL), 1u);
  StartSoftAP();
  env_->ScheduleNotification(
      std::bind(&CreateSoftAPTest::TxAuthReq, this, simulation::AUTH_TYPE_OPEN, kFakeMac),
      zx::msec(5));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::TxAssocReq, this, kFakeMac),
                             zx::msec(10));
  env_->ScheduleNotification(
      std::bind(&CreateSoftAPTest::SetExpectMacForInds, this, kSecondClientMac), zx::msec(15));
  env_->ScheduleNotification(
      std::bind(&CreateSoftAPTest::TxAuthReq, this, simulation::AUTH_TYPE_OPEN, kSecondClientMac),
      zx::msec(20));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::TxAssocReq, this, kSecondClientMac),
                             zx::msec(30));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::VerifyNumOfClient, this, 2),
                             zx::msec(40));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::SetExpectMacForInds, this, kFakeMac),
                             zx::msec(45));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::TxDeauthReq, this, kFakeMac),
                             zx::msec(50));
  env_->ScheduleNotification(std::bind(&CreateSoftAPTest::VerifyNumOfClient, this, 1),
                             zx::msec(60));
  env_->Run(kSimulatedClockDuration);
}

}  // namespace wlan::brcmfmac
