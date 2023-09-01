// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_SIM_TEST_SIM_TEST_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_SIM_TEST_SIM_TEST_H_

#include <fuchsia/wlan/common/c/banjo.h>
#include <fuchsia/wlan/fullmac/c/banjo.h>
#include <lib/async-loop/cpp/loop.h>
#include <zircon/types.h>

#include <map>

#include <zxtest/zxtest.h>

#include "src/connectivity/wlan/drivers/testing/lib/sim-device/device.h"
#include "src/connectivity/wlan/drivers/testing/lib/sim-env/sim-env.h"
#include "src/connectivity/wlan/drivers/testing/lib/sim-fake-ap/sim-fake-ap.h"
#include "src/connectivity/wlan/drivers/third_party/broadcom/brcmfmac/sim/sim_device.h"
#include "zircon/system/ulib/sync/include/lib/sync/cpp/completion.h"

namespace wlan_fullmac = fuchsia_wlan_fullmac::wire;

namespace wlan::brcmfmac {

// This class represents an interface created on a simulated device, collecting all of the
// attributes related to that interface.
class SimInterface : public fdf::WireServer<fuchsia_wlan_fullmac::WlanFullmacImplIfc> {
 public:
  // Track state of association
  struct AssocContext {
    enum AssocState {
      kNone,
      kAssociating,
      kAssociated,
    } state = kNone;

    common::MacAddr bssid;
    std::vector<uint8_t> ies;
    wlan_common::WlanChannel channel;
  };

  struct SoftApContext {
    wlan_ieee80211::CSsid ssid;
  };

  // Useful statistics about operations performed
  struct Stats {
    size_t connect_attempts = 0;
    size_t connect_successes = 0;
    std::list<wlan_fullmac::WlanFullmacConnectConfirm> connect_results;
    std::list<wlan_fullmac::WlanFullmacAssocInd> assoc_indications;
    std::list<wlan_fullmac::WlanFullmacAuthInd> auth_indications;
    std::list<wlan_fullmac::WlanFullmacDeauthConfirm> deauth_results;
    std::list<wlan_fullmac::WlanFullmacDeauthIndication> deauth_indications;
    std::list<wlan_fullmac::WlanFullmacDisassocIndication> disassoc_indications;
    std::list<wlan_fullmac::WlanFullmacChannelSwitchInfo> csa_indications;
    std::list<wlan_fullmac::WlanFullmacStartConfirm> start_confirmations;
    std::list<wlan_fullmac::WlanFullmacStopConfirm> stop_confirmations;
  };

  // Default scan options
  static const std::vector<uint8_t> kDefaultScanChannels;
  static constexpr uint32_t kDefaultActiveScanDwellTimeMs = 40;
  static constexpr uint32_t kDefaultPassiveScanDwellTimeMs = 120;

  // SoftAP defaults
  static constexpr wlan_ieee80211::CSsid kDefaultSoftApSsid = {.len = 10,
                                                               .data = {
                                                                   .data_ = "Sim_SoftAP",
                                                               }};
  static constexpr wlan_common::WlanChannel kDefaultSoftApChannel = {
      .primary = 11, .cbw = wlan_common::ChannelBandwidth::kCbw20, .secondary80 = 0};
  static constexpr uint32_t kDefaultSoftApBeaconPeriod = 100;
  static constexpr uint32_t kDefaultSoftApDtimPeriod = 100;

  SimInterface();
  SimInterface(const SimInterface&) = delete;
  ~SimInterface();

  zx_status_t Init(std::shared_ptr<simulation::Environment> env, wlan_common::WlanMacRole role);

  // This function establish connection between this object and WlanInterface instance.
  zx_status_t Connect(fdf::ClientEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> client_end);

  // Create server_dispatcher_.
  void CreateDispatcher();
  // Shutdown server_dispatcher_.
  void DestroyDispatcher();

  // Default SME Callbacks
  // Implementation of fuchsia_wlan_fullmac::WlanFullmacImplIfc.
  void OnScanResult(OnScanResultRequestView request, fdf::Arena& arena,
                    OnScanResultCompleter::Sync& completer) override;
  void OnScanEnd(OnScanEndRequestView request, fdf::Arena& arena,
                 OnScanEndCompleter::Sync& completer) override;
  void ConnectConf(ConnectConfRequestView request, fdf::Arena& arena,
                   ConnectConfCompleter::Sync& completer) override;
  void RoamConf(RoamConfRequestView request, fdf::Arena& arena,
                RoamConfCompleter::Sync& completer) override;
  void AuthInd(AuthIndRequestView request, fdf::Arena& arena,
               AuthIndCompleter::Sync& completer) override;
  void DeauthConf(DeauthConfRequestView request, fdf::Arena& arena,
                  DeauthConfCompleter::Sync& completer) override;
  void DeauthInd(DeauthIndRequestView request, fdf::Arena& arena,
                 DeauthIndCompleter::Sync& completer) override;
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
  void EapolConf(EapolConfRequestView request, fdf::Arena& arena,
                 EapolConfCompleter::Sync& completer) override;
  void OnChannelSwitch(OnChannelSwitchRequestView request, fdf::Arena& arena,
                       OnChannelSwitchCompleter::Sync& completer) override;
  void SignalReport(SignalReportRequestView request, fdf::Arena& arena,
                    SignalReportCompleter::Sync& completer) override;
  void EapolInd(EapolIndRequestView request, fdf::Arena& arena,
                EapolIndCompleter::Sync& completer) override;
  void RelayCapturedFrame(RelayCapturedFrameRequestView request, fdf::Arena& arena,
                          RelayCapturedFrameCompleter::Sync& completer) override;
  void OnPmkAvailable(OnPmkAvailableRequestView request, fdf::Arena& arena,
                      OnPmkAvailableCompleter::Sync& completer) override;
  void SaeHandshakeInd(SaeHandshakeIndRequestView request, fdf::Arena& arena,
                       SaeHandshakeIndCompleter::Sync& completer) override;
  void SaeFrameRx(SaeFrameRxRequestView request, fdf::Arena& arena,
                  SaeFrameRxCompleter::Sync& completer) override;
  void OnWmmStatusResp(OnWmmStatusRespRequestView request, fdf::Arena& arena,
                       OnWmmStatusRespCompleter::Sync& completer) override;

  // Query an interface
  void Query(wlan_fullmac::WlanFullmacQueryInfo* out_info);

  // Query for MAC sublayer feature support on an interface
  void QueryMacSublayerSupport(wlan_common::MacSublayerSupport* out_resp);

  // Query for security feature support on an interface
  void QuerySecuritySupport(wlan_common::SecuritySupport* out_resp);

  // Query for spectrum management support on an interface
  void QuerySpectrumManagementSupport(wlan_common::SpectrumManagementSupport* out_resp);

  // Stop an interface
  void StopInterface();

  // Get the Mac address of an interface
  void GetMacAddr(common::MacAddr* out_macaddr);

  // Start an assocation with a fake AP. We can use these for subsequent association events, but
  // not interleaved association events (which I doubt are terribly useful, anyway). Note that for
  // the moment only non-authenticated associations are supported.
  void StartConnect(const common::MacAddr& bssid, const wlan_ieee80211::CSsid& ssid,
                    const wlan_common::WlanChannel& channel);
  void AssociateWith(const simulation::FakeAp& ap,
                     std::optional<zx::duration> delay = std::nullopt);

  void DeauthenticateFrom(const common::MacAddr& bssid, wlan_ieee80211::ReasonCode reason);

  // Scan operations
  void StartScan(uint64_t txn_id = 0, bool active = false,
                 std::optional<const std::vector<uint8_t>> channels =
                     std::optional<const std::vector<uint8_t>>{});
  std::optional<wlan_fullmac::WlanScanResult> ScanResultCode(uint64_t txn_id);
  const std::list<wlan_fullmac::WlanFullmacScanResult>* ScanResultList(uint64_t txn_id);

  // SoftAP operation
  void StartSoftAp(const wlan_ieee80211::CSsid& ssid = kDefaultSoftApSsid,
                   const wlan_common::WlanChannel& channel = kDefaultSoftApChannel,
                   uint32_t beacon_period = kDefaultSoftApBeaconPeriod,
                   uint32_t dtim_period = kDefaultSoftApDtimPeriod);
  void StopSoftAp();

  zx_status_t SetMulticastPromisc(bool enable);

  std::shared_ptr<simulation::Environment> env_;

  fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImpl> client_;

  // Unique identifier provided by the driver
  uint16_t iface_id_;

  // Handles for SME <=> MLME communication, required but never used for communication (since no
  // SME is present).
  zx_handle_t ch_sme_ = ZX_HANDLE_INVALID;   // SME-owned side
  zx_handle_t ch_mlme_ = ZX_HANDLE_INVALID;  // MLME-owned side

  // Current state of association
  AssocContext assoc_ctx_ = {};

  // Current state of soft AP
  SoftApContext soft_ap_ctx_ = {};

  // Allows us to track individual operations
  Stats stats_ = {};

  fdf::Arena test_arena_;

 private:
  // Dispatch the FIDL request from fuchsia_wlan_fullmac::WlanFullmacImplIfc.
  fdf::Dispatcher server_dispatcher_;
  // The completion waits for the completion of the AsyncShutdown() of server_dispatcher_.
  libsync::Completion server_completion_;

  wlan_common::WlanMacRole role_;

  // Track scan results
  struct ScanStatus {
    // If not present, indicates that the scan has not completed yet
    std::optional<wlan_fullmac::WlanScanResult> result_code = std::nullopt;
    std::list<wlan_fullmac::WlanFullmacScanResult> result_list;
  };
  // One entry per scan started
  std::map<uint64_t, ScanStatus> scan_results_;
  // BSS's IEs are raw pointers. Store the IEs here so we don't have dangling pointers
  std::vector<std::vector<uint8_t>> scan_results_ies_;
};

// A base class that can be used for creating simulation tests. It provides functionality that
// should be common to most tests (like creating a new device instance and setting up and plugging
// into the environment). It also provides a factory method for creating a new interface on the
// simulated device.
class SimTest : public ::zxtest::Test, public simulation::StationIfc {
 public:
  SimTest();
  ~SimTest();

  // In some cases (like error injection that affects the initialization) we want to work with
  // an uninitialized device. This method will allocate, but not initialize the device. To complete
  // initialization, the Init() function can be called after PreInit().
  zx_status_t PreInit();

  // Allocate device (if it hasn't already been allocated) and initialize it. This function doesn't
  // require PreInit() to be called first.
  zx_status_t Init();

  std::shared_ptr<simulation::Environment> env_;

 protected:
  // Create a new interface on the simulated device, providing the specified role and function
  // callbacks
  zx_status_t StartInterface(wlan_common::WlanMacRole role, SimInterface* sim_ifc,
                             std::optional<common::MacAddr> mac_addr = std::nullopt);

  // Stop and delete a SimInterface
  zx_status_t DeleteInterface(SimInterface* ifc);

  // To notify simulator that an interface was destroyed.
  // e.g. when going through crash recovery.
  zx_status_t InterfaceDestroyed(SimInterface* sim_ifc);

  // Fake device manager
  std::unique_ptr<simulation::FakeDevMgr> dev_mgr_;

  // brcmfmac's concept of a device
  brcmfmac::SimDevice* device_ = nullptr;

  // Keep track of the ifaces we created during test by iface id.
  std::map<uint16_t, SimInterface*> ifaces_;

  fdf::WireSharedClient<fuchsia_wlan_phyimpl::WlanPhyImpl> client_;
  fidl::WireSyncClient<fuchsia_factory_wlan::Iovar> factory_device_;
  fdf::Dispatcher client_dispatcher_;
  // The dispatcher to manage the lifecycle of devices.
  fdf::Dispatcher driver_dispatcher_;
  fdf::Arena test_arena_;
  libsync::Completion completion_;
  libsync::Completion client_completion_;

 private:
  // StationIfc methods - by default, do nothing. These can/will be overridden by superclasses.
  void Rx(std::shared_ptr<const simulation::SimFrame> frame,
          std::shared_ptr<const simulation::WlanRxInfo> info) override {}
  async::Loop loop_;
};

}  // namespace wlan::brcmfmac

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_BROADCOM_BRCMFMAC_SIM_TEST_SIM_TEST_H_
