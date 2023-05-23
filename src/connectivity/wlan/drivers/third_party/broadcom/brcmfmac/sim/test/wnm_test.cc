// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fuchsia/hardware/wlan/fullmac/c/banjo.h>
#include <fuchsia/wlan/common/c/banjo.h>
#include <zircon/errors.h>

#include <memory>

#include "src/connectivity/wlan/drivers/testing/lib/sim-env/sim-frame.h"
#include "src/connectivity/wlan/drivers/testing/lib/sim-fake-ap/sim-fake-ap.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/feature.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"
#include "src/connectivity/wlan/lib/common/cpp/include/wlan/common/macaddr.h"

namespace wlan::brcmfmac {

// Some default AP and association request values
constexpr cssid_t kDefaultSsid = {.len = 15, .data = "Fuchsia Fake AP"};

constexpr wlan_channel_t kAp0Channel = {
    .primary = 9, .cbw = CHANNEL_BANDWIDTH_CBW20, .secondary80 = 0};
constexpr wlan_channel_t kAp1Channel = {
    .primary = 11, .cbw = CHANNEL_BANDWIDTH_CBW20, .secondary80 = 0};
const uint8_t kAp1OperatingClass = 101;

const common::MacAddr kAp0Bssid("12:34:56:78:9a:bc");
const common::MacAddr kAp1Bssid("ff:ee:dd:cc:bb:aa");

class WnmTest : public SimTest {
 public:
  // How long an individual test will run for. We need an end time because tests run until no more
  // events remain and so we need to stop aps from beaconing to drain the event queue.
  static constexpr zx::duration kTestDuration = zx::sec(100);

  // If a test needs firmware BTM enabled in the driver, this PreInit must be called.
  void PreInit();

  void Init();

  // Schedule a future BSS Transition Management request event.
  void ScheduleBtmReq(const simulation::SimBtmReqFrame& btm_req, zx::duration when);

 protected:
  // This is the interface we will use for our single client interface
  SimInterface client_ifc_;
  std::list<simulation::FakeAp*> aps_;
  // If set to true, the driver will be initialized with firmware BTM features.
  bool setup_btm_firmware_support_ = false;

  size_t btm_req_frame_count_ = 0;

 private:
  // Stationifc override
  void Rx(std::shared_ptr<const simulation::SimFrame> frame,
          std::shared_ptr<const simulation::WlanRxInfo> info) override;
};

// Set up the driver feature flags before the device is created.
void WnmTest::PreInit() {
  ASSERT_EQ(SimTest::PreInit(), ZX_OK);
  if (setup_btm_firmware_support_) {
    device_->GetSim()->drvr->feat_flags |= BIT(BRCMF_FEAT_ROAM_ENGINE);
    device_->GetSim()->drvr->feat_flags |= BIT(BRCMF_FEAT_WNM_BTM);
  } else {
    device_->GetSim()->drvr->feat_flags &= !(BIT(BRCMF_FEAT_ROAM_ENGINE));
    device_->GetSim()->drvr->feat_flags &= !(BIT(BRCMF_FEAT_WNM_BTM));
  }
  // Set to false here to prevent this from being enabled inadvertently in future tests.
  setup_btm_firmware_support_ = false;
}

// Create our device instance and hook up the callbacks
void WnmTest::Init() {
  ASSERT_EQ(SimTest::Init(), ZX_OK);

  ASSERT_EQ(StartInterface(WLAN_MAC_ROLE_CLIENT, &client_ifc_), ZX_OK);
  btm_req_frame_count_ = 0;
}

// This function schedules a BTM req frame sent from the first AP.
void WnmTest::ScheduleBtmReq(const simulation::SimBtmReqFrame& btm_req, zx::duration when) {
  BRCMF_INFO("Scheduling BTM");
  ZX_ASSERT_MSG(!aps_.empty(), "Cannot sent BTM req because there is no AP\n");
  env_->ScheduleNotification([this, btm_req] { aps_.front()->SendBtmReq(btm_req); }, when);
}

void WnmTest::Rx(std::shared_ptr<const simulation::SimFrame> frame,
                 std::shared_ptr<const simulation::WlanRxInfo> info) {
  ASSERT_EQ(frame->FrameType(), simulation::SimFrame::FRAME_TYPE_MGMT);
  auto mgmt_frame = std::static_pointer_cast<const simulation::SimManagementFrame>(frame);

  if (mgmt_frame->MgmtFrameType() != simulation::SimManagementFrame::FRAME_TYPE_ACTION) {
    return;
  }
  auto action_frame = std::static_pointer_cast<const simulation::SimActionFrame>(mgmt_frame);
  if (action_frame->ActionCategory() != simulation::SimActionFrame::SimActionCategory::WNM) {
    return;
  }
  auto wnm_action_frame =
      std::static_pointer_cast<const simulation::SimWnmActionFrame>(action_frame);
  if (wnm_action_frame->WnmAction() !=
      simulation::SimWnmActionFrame::SimWnmAction::BSS_TRANSITION_MANAGEMENT_REQUEST) {
    return;
  }
  ++btm_req_frame_count_;
}

TEST_F(WnmTest, IgnoreBtmReqWhenBtmUnsupported) {
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
  const simulation::SimBtmReqMode req_mode{.preferred_candidate_list_included = true};
  const simulation::SimNeighborReportElement neighbor{
      .bssid = kAp1Bssid,
      .operating_class = kAp1OperatingClass,
      .channel_number = kAp1Channel.primary,
  };
  const std::vector<simulation::SimNeighborReportElement> candidates({neighbor});
  const simulation::SimBtmReqFrame btm_req(kAp0Bssid, client_mac, req_mode, candidates);
  ScheduleBtmReq(btm_req, zx::sec(1));
  env_->Run(kTestDuration);

  EXPECT_EQ(SimInterface::AssocContext::kAssociated, client_ifc_.assoc_ctx_.state);
  EXPECT_EQ(kAp0Bssid, client_ifc_.assoc_ctx_.bssid);
  EXPECT_EQ(1U, client_ifc_.stats_.connect_attempts);
  EXPECT_EQ(1U, ap_0.GetNumAssociatedClient());
  EXPECT_EQ(0U, ap_1.GetNumAssociatedClient());
  EXPECT_EQ(1U, btm_req_frame_count_);
}

// DUT is configured to roam when AP sends a BTM request. In this test the target
// AP will respond with reassociation success and the DUT should roam successfully.
TEST_F(WnmTest, RoamOnBtmReqWhenConfiguredToRoam) {
  setup_btm_firmware_support_ = true;
  PreInit();
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
  const simulation::SimBtmReqMode req_mode{.preferred_candidate_list_included = true};
  const simulation::SimNeighborReportElement neighbor{
      .bssid = kAp1Bssid,
      .operating_class = kAp1OperatingClass,
      .channel_number = kAp1Channel.primary,
  };
  const std::vector<simulation::SimNeighborReportElement> candidates({neighbor});
  const simulation::SimBtmReqFrame btm_req(kAp0Bssid, client_mac, req_mode, candidates);
  ScheduleBtmReq(btm_req, zx::sec(1));
  env_->Run(kTestDuration);

  EXPECT_EQ(SimInterface::AssocContext::kAssociated, client_ifc_.assoc_ctx_.state);
  EXPECT_EQ(kAp1Bssid, client_ifc_.assoc_ctx_.bssid);
  EXPECT_EQ(1U, client_ifc_.stats_.connect_attempts);
  // We don't check that the STA is not associated with the previous Sim AP,
  // because Sim AP will maintain the association indefinitely.
  EXPECT_EQ(1U, ap_1.GetNumAssociatedClient());
  EXPECT_EQ(1U, btm_req_frame_count_);
}

// DUT is configured to roam when AP sends a BTM request, but the target AP will ignore the
// reassociation. This test exercises the roam timeout functionality in the driver.
TEST_F(WnmTest, RoamOnBtmReqButTargetApIgnoresReassoc) {
  setup_btm_firmware_support_ = true;
  PreInit();
  Init();

  simulation::FakeAp ap_0(env_.get(), kAp0Bssid, kDefaultSsid, kAp0Channel);
  simulation::FakeAp ap_1(env_.get(), kAp1Bssid, kDefaultSsid, kAp1Channel);
  ap_0.EnableBeacon(zx::msec(60));
  ap_1.EnableBeacon(zx::msec(60));
  // This AP will ignore reassociation, to test roam failure.
  ap_1.SetAssocHandling(simulation::FakeAp::ASSOC_IGNORED);
  aps_.push_back(&ap_0);
  aps_.push_back(&ap_1);

  client_ifc_.AssociateWith(ap_0, zx::msec(10));

  common::MacAddr client_mac;
  client_ifc_.GetMacAddr(&client_mac);
  const simulation::SimBtmReqMode req_mode{.preferred_candidate_list_included = true};
  const simulation::SimNeighborReportElement neighbor{
      .bssid = kAp1Bssid,
      .operating_class = kAp1OperatingClass,
      .channel_number = kAp1Channel.primary,
  };
  const std::vector<simulation::SimNeighborReportElement> candidates({neighbor});
  const simulation::SimBtmReqFrame btm_req(kAp0Bssid, client_mac, req_mode, candidates);
  ScheduleBtmReq(btm_req, zx::sec(1));
  env_->Run(kTestDuration);

  EXPECT_EQ(SimInterface::AssocContext::kAssociated, client_ifc_.assoc_ctx_.state);
  EXPECT_EQ(kAp0Bssid, client_ifc_.assoc_ctx_.bssid);
  EXPECT_EQ(1U, client_ifc_.stats_.connect_attempts);
  EXPECT_EQ(1U, ap_0.GetNumAssociatedClient());
  EXPECT_EQ(0U, ap_1.GetNumAssociatedClient());
  EXPECT_EQ(1U, btm_req_frame_count_);
}

}  // namespace wlan::brcmfmac
