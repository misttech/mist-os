// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <arpa/inet.h>
#include <fuchsia/hardware/wlan/fullmac/c/banjo.h>
#include <fuchsia/wlan/ieee80211/cpp/fidl.h>
#include <zircon/errors.h>

#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/testing/lib/sim-device/device.h"
#include "src/connectivity/wlan/drivers/testing/lib/sim-env/sim-env.h"
#include "src/connectivity/wlan/drivers/testing/lib/sim-fake-ap/sim-fake-ap.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/cfg80211.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/feature.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim_data_path.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim_utils.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/test/sim_test.h"
#include "src/connectivity/wlan/lib/common/cpp/include/wlan/common/macaddr.h"

namespace wlan::brcmfmac {

namespace {
#define OUR_MAC \
  { 0xff, 0xfe, 0xfd, 0xfc, 0xfb, 0xfa }
#define THEIR_MAC \
  { 0xde, 0xad, 0xbe, 0xef, 0x00, 0x02 }
const common::MacAddr kOurMac(OUR_MAC);
const common::MacAddr kTheirMac(THEIR_MAC);

// A simple ARP request
const ether_arp kSampleArpReq = {.ea_hdr = {.ar_hrd = htons(ETH_P_802_3),
                                            .ar_pro = htons(ETH_P_IP),
                                            .ar_hln = 6,
                                            .ar_pln = 4,
                                            .ar_op = htons(ARPOP_REQUEST)},
                                 .arp_sha = OUR_MAC,
                                 .arp_spa = {192, 168, 42, 11},
                                 .arp_tha = THEIR_MAC,
                                 .arp_tpa = {100, 101, 102, 103}};

const std::vector<uint8_t> kDummyData = {0, 1, 2, 3, 4};

void VerifyArpFrame(const std::vector<uint8_t>& frame) {
  const ethhdr* eth_hdr = reinterpret_cast<const ethhdr*>(frame.data());
  EXPECT_BYTES_EQ(eth_hdr->h_dest, common::kBcastMac.byte, ETH_ALEN);
  EXPECT_BYTES_EQ(eth_hdr->h_source, kTheirMac.byte, ETH_ALEN);
  EXPECT_EQ(ntohs(eth_hdr->h_proto), ETH_P_ARP);

  const ether_arp* arp_hdr = reinterpret_cast<const ether_arp*>(frame.data() + sizeof(ethhdr));
  EXPECT_EQ(memcmp(arp_hdr, &kSampleArpReq, sizeof(ether_arp)), 0);
}

void VerifyNonArpFrame(const std::vector<uint8_t>& frame) {
  // For this test only, assume anything that is the right size is an ARP frame
  ASSERT_NE(frame.size(), sizeof(ethhdr) + sizeof(ether_arp));
  ASSERT_EQ(frame.size(), sizeof(ethhdr) + kDummyData.size());
  EXPECT_BYTES_EQ(kDummyData.data(), &frame[sizeof(ethhdr)], kDummyData.size());
}

}  // namespace

struct GenericIfc : public SimInterface {
  void OnAssocInd(const wlan_fullmac_assoc_ind_t* ind) override;
  void OnDataRecv(const void* frame, size_t frame_size, uint32_t flags) override;
  void OnStartConf(const wlan_fullmac_start_confirm_t* resp) override;
  void OnStopConf(const wlan_fullmac_stop_confirm_t* resp) override;

  bool assoc_ind_recv_ = false;
};

class ArpTest : public SimTest {
 public:
  static constexpr zx::duration kTestDuration = zx::sec(100);

  ArpTest() = default;
  void Init();
  void CleanupApInterface();

  // Send a frame directly into the environment
  void Tx(const std::vector<uint8_t>& ethFrame);

  // Interface management
  zx_status_t SetMulticastPromisc(bool enable);
  void StartAndStopSoftAP();

  // Simulation of client associating to a SoftAP interface
  void TxAuthandAssocReq();
  void VerifyAssoc();

  // Frame processing
  void ScheduleArpFrameTx(zx::duration when, bool expect_rx);
  void ScheduleNonArpFrameTx(zx::duration when);

 protected:
  std::vector<uint8_t> ethFrame;
  GenericIfc sim_ifc_;
};

void GenericIfc::OnAssocInd(const wlan_fullmac_assoc_ind_t* ind) {
  ASSERT_EQ(std::memcmp(ind->peer_sta_address, kTheirMac.byte, ETH_ALEN), 0);
  assoc_ind_recv_ = true;
}

void GenericIfc::OnDataRecv(const void* frame, size_t size, uint32_t flags) {
  // Should not be called
  FAIL();
}

void GenericIfc::OnStartConf(const wlan_fullmac_start_confirm_t* resp) {
  ASSERT_EQ(resp->result_code, WLAN_START_RESULT_SUCCESS);
}

void GenericIfc::OnStopConf(const wlan_fullmac_stop_confirm_t* resp) {
  ASSERT_EQ(resp->result_code, WLAN_STOP_RESULT_SUCCESS);
}

void ArpTest::Init() { ASSERT_EQ(SimTest::Init(), ZX_OK); }

zx_status_t ArpTest::SetMulticastPromisc(bool enable) {
  const wlan_fullmac_impl_protocol_ops_t* ops = sim_ifc_.if_impl_ops_;
  void* ctx = sim_ifc_.if_impl_ctx_;
  return ops->set_multicast_promisc(ctx, enable);
}

void ArpTest::TxAuthandAssocReq() {
  // Get the mac address of the SoftAP
  const common::MacAddr mac(kTheirMac);
  simulation::WlanTxInfo tx_info = {.channel = SimInterface::kDefaultSoftApChannel};
  simulation::SimAuthFrame auth_req_frame(mac, kOurMac, 1, simulation::AUTH_TYPE_OPEN,
                                          ::fuchsia::wlan::ieee80211::StatusCode::SUCCESS);
  env_->Tx(auth_req_frame, tx_info, this);
  simulation::SimAssocReqFrame assoc_req_frame(mac, kOurMac, SimInterface::kDefaultSoftApSsid);
  env_->Tx(assoc_req_frame, tx_info, this);
}

void ArpTest::VerifyAssoc() {
  // Verify the event indications were received and
  // the number of clients
  ASSERT_EQ(sim_ifc_.assoc_ind_recv_, true);
  brcmf_simdev* sim = device_->GetSim();
  uint16_t num_clients = sim->sim_fw->GetNumClients(sim_ifc_.iface_id_);
  ASSERT_EQ(num_clients, 1U);
}

void ArpTest::CleanupApInterface() {
  sim_ifc_.StopSoftAp();
  EXPECT_EQ(DeleteInterface(&sim_ifc_), ZX_OK);
}

void ArpTest::Tx(const std::vector<uint8_t>& ethFrame) {
  const ethhdr* eth_hdr = reinterpret_cast<const ethhdr*>(ethFrame.data());
  common::MacAddr dst(eth_hdr->h_dest);
  common::MacAddr src(eth_hdr->h_source);
  simulation::SimQosDataFrame dataFrame(true, false, dst, src, common::kBcastMac, 0, ethFrame);
  simulation::WlanTxInfo tx_info = {.channel = SimInterface::kDefaultSoftApChannel};
  env_->Tx(dataFrame, tx_info, this);
}

void ArpTest::ScheduleArpFrameTx(zx::duration when, bool expect_rx) {
  ether_arp arp_frame = kSampleArpReq;

  cpp20::span<const uint8_t> body{reinterpret_cast<const uint8_t*>(&arp_frame), sizeof(arp_frame)};
  std::vector<uint8_t> frame_bytes =
      sim_utils::CreateEthernetFrame(common::kBcastMac, kTheirMac, ETH_P_ARP, body);
  env_->ScheduleNotification(std::bind(&ArpTest::Tx, this, frame_bytes), when);
}

void ArpTest::ScheduleNonArpFrameTx(zx::duration when) {
  std::vector<uint8_t> frame_bytes =
      sim_utils::CreateEthernetFrame(kOurMac, kTheirMac, 0, kDummyData);
  env_->ScheduleNotification(std::bind(&ArpTest::Tx, this, frame_bytes), when);
}

void ArpTest::StartAndStopSoftAP() {
  common::MacAddr ap_mac({0xfa, 0xfb, 0xfc, 0xfd, 0xfe, 0xff});
  GenericIfc softap_ifc;
  ASSERT_EQ(SimTest::StartInterface(WLAN_MAC_ROLE_AP, &softap_ifc, std::nullopt, ap_mac), ZX_OK);
  softap_ifc.StartSoftAp();
  softap_ifc.StopSoftAp();
}

// Verify that an ARP frame received by an AP interface is not offloaded, even after multicast
// promiscuous mode is enabled.
TEST_F(ArpTest, SoftApArpOffload) {
  Init();
  ASSERT_EQ(SimTest::StartInterface(WLAN_MAC_ROLE_AP, &sim_ifc_, std::nullopt, kOurMac), ZX_OK);
  sim_ifc_.StartSoftAp();

  // Have the test associate with the AP
  env_->ScheduleNotification(std::bind(&ArpTest::TxAuthandAssocReq, this), zx::sec(1));
  env_->ScheduleNotification(std::bind(&ArpTest::VerifyAssoc, this), zx::sec(2));

  // Send an ARP frame that we expect to be received
  ScheduleArpFrameTx(zx::sec(3), true);
  ScheduleNonArpFrameTx(zx::sec(4));

  env_->ScheduleNotification(std::bind(&ArpTest::SetMulticastPromisc, this, true), zx::sec(5));

  // Send an ARP frame that we expect to be received
  ScheduleArpFrameTx(zx::sec(6), true);
  ScheduleNonArpFrameTx(zx::sec(7));

  // Stop AP and remove interface
  env_->ScheduleNotification(std::bind(&ArpTest::CleanupApInterface, this), zx::sec(8));

  env_->Run(kTestDuration);

  // Verify that no ARP frames were offloaded and that no non-ARP frames were suppressed
  auto& received = device_->DataPath().RxData();
  ASSERT_EQ(received.size(), 4);

  VerifyArpFrame(received[0]);
  VerifyNonArpFrame(received[1]);
  VerifyArpFrame(received[2]);
  VerifyNonArpFrame(received[3]);
}

// On a client interface, we expect no ARP frames to be offloaded to firmware, regardless of
// the multicast promiscuous setting, since SoftAP feature disables ARP offload by default.
TEST_F(ArpTest, ClientArpOffload) {
  Init();

  ASSERT_EQ(SimTest::StartInterface(WLAN_MAC_ROLE_CLIENT, &sim_ifc_, std::nullopt, kOurMac), ZX_OK);

  // Start a fake AP
  simulation::FakeAp ap(env_.get(), kTheirMac, SimInterface::kDefaultSoftApSsid,
                        SimInterface::kDefaultSoftApChannel);

  // Associate with fake AP
  sim_ifc_.AssociateWith(ap, zx::sec(1));

  // Send an ARP frame that we expect to receive (not get offloaded)
  ScheduleArpFrameTx(zx::sec(2), false);
  ScheduleNonArpFrameTx(zx::sec(3));

  env_->ScheduleNotification(std::bind(&ArpTest::SetMulticastPromisc, this, true), zx::sec(4));

  // Send another ARP frame that we expect to receive (not get offloaded)
  ScheduleArpFrameTx(zx::sec(5), false);
  ScheduleNonArpFrameTx(zx::sec(6));

  env_->Run(kTestDuration);

  // Verify that we completed the association process
  EXPECT_EQ(sim_ifc_.stats_.connect_successes, 1U);

  // Verify that no ARP frames were offloaded and that no non-ARP frames were suppressed
  auto& received = device_->DataPath().RxData();
  ASSERT_EQ(received.size(), 4);

  VerifyArpFrame(received[0]);
  VerifyNonArpFrame(received[1]);
  VerifyArpFrame(received[2]);
  VerifyNonArpFrame(received[3]);
}

// Start and Stop of SoftAP should not affect ARP OL configured by client.
TEST_F(ArpTest, SoftAPStartStopDoesNotAffectArpOl) {
  Init();

  ASSERT_EQ(SimTest::StartInterface(WLAN_MAC_ROLE_CLIENT, &sim_ifc_, std::nullopt, kOurMac), ZX_OK);

  // Start a fake AP
  simulation::FakeAp ap(env_.get(), kTheirMac, SimInterface::kDefaultSoftApSsid,
                        SimInterface::kDefaultSoftApChannel);

  // Associate with fake AP
  sim_ifc_.AssociateWith(ap, zx::sec(1));

  // Send an ARP frame that we expect to receive (not get offloaded)
  ScheduleArpFrameTx(zx::sec(2), false);
  ScheduleNonArpFrameTx(zx::sec(3));

  // Start and Stop SoftAP. This should not affect ARP Ol
  env_->ScheduleNotification(std::bind(&ArpTest::StartAndStopSoftAP, this), zx::sec(4));

  // Send another ARP frame that we expect to receive (not get offloaded)
  ScheduleArpFrameTx(zx::sec(5), false);
  ScheduleNonArpFrameTx(zx::sec(6));

  env_->Run(kTestDuration);

  // Verify that we completed the association process
  EXPECT_EQ(sim_ifc_.stats_.connect_successes, 1U);

  // Verify that no ARP frames were offloaded and that no non-ARP frames were suppressed
  auto& received = device_->DataPath().RxData();
  ASSERT_EQ(received.size(), 4);

  VerifyArpFrame(received[0]);
  VerifyNonArpFrame(received[1]);
  VerifyArpFrame(received[2]);
  VerifyNonArpFrame(received[3]);
}

// On a client interface, we expect all ARP frames to be offloaded to firmware, regardless of
// the multicast promiscuous setting, when SoftAP feature is not available.
TEST_F(ArpTest, ClientArpOffloadNoSoftApFeat) {
  Init();

  // We disable SoftAP feature, so that our driver enabled Arp offload.
  device_->GetSim()->drvr->feat_flags &= (!BIT(BRCMF_FEAT_AP));

  ASSERT_EQ(SimTest::StartInterface(WLAN_MAC_ROLE_CLIENT, &sim_ifc_, std::nullopt, kOurMac), ZX_OK);

  // Start a fake AP
  simulation::FakeAp ap(env_.get(), kTheirMac, SimInterface::kDefaultSoftApSsid,
                        SimInterface::kDefaultSoftApChannel);

  // Associate with fake AP
  sim_ifc_.AssociateWith(ap, zx::sec(1));

  // Send an ARP frame that we expect to be offloaded
  ScheduleArpFrameTx(zx::sec(2), false);
  ScheduleNonArpFrameTx(zx::sec(3));

  env_->ScheduleNotification(std::bind(&ArpTest::SetMulticastPromisc, this, true), zx::sec(4));

  // Send an ARP frame that we expect to be offloaded
  ScheduleArpFrameTx(zx::sec(5), false);
  ScheduleNonArpFrameTx(zx::sec(6));

  env_->Run(kTestDuration);

  // Verify that we completed the association process
  EXPECT_EQ(sim_ifc_.stats_.connect_successes, 1U);

  // Verify that all ARP frames were offloaded and that no non-ARP frames were suppressed
  auto& received = device_->DataPath().RxData();
  ASSERT_EQ(received.size(), 2);
  VerifyNonArpFrame(received[0]);
  VerifyNonArpFrame(received[1]);
}

}  // namespace wlan::brcmfmac
