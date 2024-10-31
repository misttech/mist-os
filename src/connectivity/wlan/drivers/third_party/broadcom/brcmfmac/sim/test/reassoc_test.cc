// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fidl/fuchsia.wlan.fullmac/cpp/wire_types.h"
#include "src/connectivity/wlan/drivers/testing/lib/sim-env/sim-frame.h"
#include "src/connectivity/wlan/drivers/testing/lib/sim-fake-ap/sim-fake-ap.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/fwil.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"
#include "src/connectivity/wlan/lib/common/cpp/include/wlan/common/macaddr.h"
#include "src/devices/lib/broadcom/include/wifi/wifi-config.h"
#include "zircon/errors.h"

namespace wlan::brcmfmac {

// Some default AP and association request values
constexpr wlan_ieee80211::CSsid kDefaultSsid = {.len = 15, .data = {.data_ = "Fuchsia Fake AP"}};

const wlan_common::WlanChannel kAp0Channel = {
    .primary = 9, .cbw = wlan_common::ChannelBandwidth::kCbw20, .secondary80 = 0};
const wlan_common::WlanChannel kAp1Channel = {
    .primary = 11, .cbw = wlan_common::ChannelBandwidth::kCbw20, .secondary80 = 0};
const simulation::WlanTxInfo kAp0TxInfo = {.channel = kAp0Channel};

const common::MacAddr kAp0Bssid("12:34:56:78:9a:bc");
const common::MacAddr kAp1Bssid("ff:ee:dd:cc:bb:aa");

class ReassocTest : public SimTest {
 public:
  // How long an individual test will run for. We need an end time because tests run until no more
  // events remain and so we need to stop aps from beaconing to drain the event queue.
  static constexpr zx::duration kTestDuration = zx::sec(100);

  void Init();

  // Schedule a future reassoc response event.
  void ScheduleReassocResp(const simulation::SimReassocRespFrame& reassoc_resp,
                           const simulation::WlanTxInfo& tx_info, zx::duration when);

  // Schedule a future roam request.
  void ScheduleRoam(const simulation::FakeAp& ap, zx::duration when);

 protected:
  // This is the interface we will use for our single client interface
  SimInterface client_ifc_;
  std::list<simulation::FakeAp*> aps_;
};

// Create our device instance and hook up the callbacks
void ReassocTest::Init() {
  ASSERT_EQ(SimTest::Init(), ZX_OK);
  ASSERT_EQ(StartInterface(wlan_common::WlanMacRole::kClient, &client_ifc_), ZX_OK);
}

// This function schedules a reassoc response frame sent from an AP.
void ReassocTest::ScheduleReassocResp(const simulation::SimReassocRespFrame& reassoc_resp,
                                      const simulation::WlanTxInfo& tx_info, zx::duration when) {
  env_->ScheduleNotification(
      [this, reassoc_resp, tx_info] { env_->Tx(reassoc_resp, tx_info, this); }, when);
}

void ReassocTest::ScheduleRoam(const simulation::FakeAp& ap, zx::duration when) {
  env_->ScheduleNotification([this, ap] { client_ifc_.StartRoam(kAp1Bssid, ap.GetChannel()); },
                             when, nullptr);
}

TEST_F(ReassocTest, RoamSucceeds) {
  Init();

  simulation::FakeAp ap_0(env_.get(), kAp0Bssid, kDefaultSsid, kAp0Channel);
  simulation::FakeAp ap_1(env_.get(), kAp1Bssid, kDefaultSsid, kAp1Channel);
  ap_0.EnableBeacon(zx::msec(60));
  ap_1.EnableBeacon(zx::msec(60));
  aps_.push_back(&ap_0);
  aps_.push_back(&ap_1);

  client_ifc_.AssociateWith(ap_0, zx::sec(1));

  common::MacAddr client_mac;
  client_ifc_.GetMacAddr(&client_mac);

  ScheduleRoam(ap_1, zx::sec(3));
  env_->Run(kTestDuration);

  EXPECT_EQ(1U, client_ifc_.stats_.roam_attempts);
  EXPECT_EQ(1U, client_ifc_.stats_.roam_successes);
  // Check that there were not multiple connects.
  EXPECT_EQ(1U, client_ifc_.stats_.connect_attempts);
  EXPECT_EQ(SimInterface::AssocContext::kAssociated, client_ifc_.assoc_ctx_.state);
  EXPECT_EQ(kAp1Bssid, client_ifc_.assoc_ctx_.bssid);
  EXPECT_EQ(1U, ap_1.GetNumAssociatedClient());
}

TEST_F(ReassocTest, IgnoreSpuriousReassocResp) {
  Init();

  simulation::FakeAp ap_0(env_.get(), kAp0Bssid, kDefaultSsid, kAp0Channel);
  simulation::FakeAp ap_1(env_.get(), kAp1Bssid, kDefaultSsid, kAp1Channel);
  ap_0.EnableBeacon(zx::msec(60));
  ap_1.EnableBeacon(zx::msec(60));
  aps_.push_back(&ap_0);
  aps_.push_back(&ap_1);

  client_ifc_.AssociateWith(ap_0, zx::msec(10));

  common::MacAddr client_mac;
  client_ifc_.GetMacAddr(&client_mac);
  // Intentionally create a response frame that never had a corresponding request.
  simulation::SimReassocRespFrame reassoc_resp(kAp0Bssid, client_mac,
                                               wlan_ieee80211::StatusCode::kSuccess);
  ScheduleReassocResp(reassoc_resp, kAp0TxInfo, zx::sec(1));
  env_->Run(kTestDuration);

  EXPECT_EQ(0U, client_ifc_.stats_.roam_attempts);
  EXPECT_EQ(0U, client_ifc_.stats_.roam_successes);
  // Check that there were not multiple connects.
  EXPECT_EQ(1U, client_ifc_.stats_.connect_attempts);
  EXPECT_EQ(SimInterface::AssocContext::kAssociated, client_ifc_.assoc_ctx_.state);
  EXPECT_EQ(kAp0Bssid, client_ifc_.assoc_ctx_.bssid);
  EXPECT_EQ(1U, ap_0.GetNumAssociatedClient());
  EXPECT_EQ(0U, ap_1.GetNumAssociatedClient());
}

TEST_F(ReassocTest, RoamTimeoutWhenNoReassocResponseReceived) {
  Init();

  simulation::FakeAp ap_0(env_.get(), kAp0Bssid, kDefaultSsid, kAp0Channel);
  simulation::FakeAp ap_1(env_.get(), kAp1Bssid, kDefaultSsid, kAp1Channel);
  ap_0.EnableBeacon(zx::msec(60));
  ap_1.EnableBeacon(zx::msec(60));
  // This AP will ignore reassociation, to test roam failure.
  ap_1.SetAssocHandling(simulation::FakeAp::ASSOC_IGNORED);
  aps_.push_back(&ap_0);
  aps_.push_back(&ap_1);

  client_ifc_.AssociateWith(ap_0, zx::sec(1));

  common::MacAddr client_mac;
  client_ifc_.GetMacAddr(&client_mac);

  ScheduleRoam(ap_1, zx::sec(3));
  env_->Run(kTestDuration);

  // Make sure the expected roam attempt failed.
  EXPECT_EQ(1U, client_ifc_.stats_.roam_attempts);
  EXPECT_EQ(0U, client_ifc_.stats_.roam_successes);
  // Check that there were not multiple connects.
  EXPECT_EQ(1U, client_ifc_.stats_.connect_attempts);
  EXPECT_EQ(SimInterface::AssocContext::kNone, client_ifc_.assoc_ctx_.state);
  EXPECT_EQ(0U, ap_0.GetNumAssociatedClient());
  EXPECT_EQ(0U, ap_1.GetNumAssociatedClient());
}

TEST_F(ReassocTest, DisconnectOnFirmwareReassocCommandFailure) {
  Init();

  simulation::FakeAp ap_0(env_.get(), kAp0Bssid, kDefaultSsid, kAp0Channel);
  simulation::FakeAp ap_1(env_.get(), kAp1Bssid, kDefaultSsid, kAp1Channel);
  ap_0.EnableBeacon(zx::msec(60));
  ap_1.EnableBeacon(zx::msec(60));
  aps_.push_back(&ap_0);
  aps_.push_back(&ap_1);

  // Inject firmware error to REASSOC command.
  WithSimDevice([this](brcmfmac::SimDevice* device) {
    brcmf_simdev* sim = device->GetSim();
    sim->sim_fw->err_inj_.AddErrInjCmd(BRCMF_C_REASSOC, ZX_ERR_NOT_SUPPORTED, BCME_UNSUPPORTED,
                                       client_ifc_.iface_id_);
  });
  client_ifc_.AssociateWith(ap_0, zx::sec(1));

  common::MacAddr client_mac;
  client_ifc_.GetMacAddr(&client_mac);

  ScheduleRoam(ap_1, zx::sec(3));
  env_->Run(kTestDuration);

  // Make sure the expected roam attempt failed.
  EXPECT_EQ(1U, client_ifc_.stats_.roam_attempts);
  EXPECT_EQ(0U, client_ifc_.stats_.roam_successes);
  // Check that there were not multiple connects.
  EXPECT_EQ(1U, client_ifc_.stats_.connect_attempts);
  EXPECT_EQ(SimInterface::AssocContext::kNone, client_ifc_.assoc_ctx_.state);
  EXPECT_EQ(0U, ap_0.GetNumAssociatedClient());
  EXPECT_EQ(0U, ap_1.GetNumAssociatedClient());
}

TEST_F(ReassocTest, DisconnectOnRoamSuccessWhenDriverCannotSyncChannel) {
  Init();

  simulation::FakeAp ap_0(env_.get(), kAp0Bssid, kDefaultSsid, kAp0Channel);
  simulation::FakeAp ap_1(env_.get(), kAp1Bssid, kDefaultSsid, kAp1Channel);
  ap_0.EnableBeacon(zx::msec(60));
  ap_1.EnableBeacon(zx::msec(60));
  aps_.push_back(&ap_0);
  aps_.push_back(&ap_1);

  // Inject firmware error to simulate firmware busy error.
  WithSimDevice([this](brcmfmac::SimDevice* device) {
    brcmf_simdev* sim = device->GetSim();
    sim->sim_fw->err_inj_.AddErrInjIovar("chanspec", ZX_ERR_SHOULD_WAIT, BCME_BUSY,
                                         client_ifc_.iface_id_);
  });
  client_ifc_.AssociateWith(ap_0, zx::sec(1));

  common::MacAddr client_mac;
  client_ifc_.GetMacAddr(&client_mac);

  ScheduleRoam(ap_1, zx::sec(3));
  env_->Run(kTestDuration);

  // Make sure the expected roam attempt failed.
  EXPECT_EQ(1U, client_ifc_.stats_.roam_attempts);
  EXPECT_EQ(0U, client_ifc_.stats_.roam_successes);
  // Check that there were not multiple connects.
  EXPECT_EQ(1U, client_ifc_.stats_.connect_attempts);
  EXPECT_EQ(SimInterface::AssocContext::kNone, client_ifc_.assoc_ctx_.state);
  // Current implementation only sends disconnect for original BSS in this scenario.
  EXPECT_EQ(0U, ap_0.GetNumAssociatedClient());
}

}  // namespace wlan::brcmfmac
