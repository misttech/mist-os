// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_WLANTAP_PHY_H_
#define SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_WLANTAP_PHY_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.wlan.tap/cpp/wire.h>
#include <lib/async/dispatcher.h>
#include <lib/driver/logging/cpp/logger.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>

#include "wlantap-mac.h"

namespace wlan_tap = fuchsia_wlan_tap::wire;

namespace wlan {

// Serves the WlantapPhy protocol, which allows the test suite to interact with the mock driver.
// This also implements the WlantapMac::Listener interface, which sends events to the test suite
// when specific WlanSoftmac calls have been made.
class WlantapPhy : public fidl::WireServer<fuchsia_wlan_tap::WlantapPhy>,
                   public WlantapMac::Listener {
 public:
  WlantapPhy(fdf::Logger* logger, zx::channel user_channel,
             std::shared_ptr<wlan_tap::WlantapPhyConfig> phy_config,
             fidl::ClientEnd<fuchsia_driver_framework::NodeController> phy_controller);

  zx_status_t SetCountry(wlan_tap::SetCountryArgs args);

  // WlantapPhy protocol implementation
  void Shutdown(ShutdownCompleter::Sync& completer) override;
  void Rx(RxRequestView request, RxCompleter::Sync& completer) override;
  void ReportTxResult(ReportTxResultRequestView request,
                      ReportTxResultCompleter::Sync& completer) override;
  void ScanComplete(ScanCompleteRequestView request,
                    ScanCompleteCompleter::Sync& completer) override;

  // WlantapMac::Listener impl
  void WlantapMacStart(fdf::ClientEnd<fuchsia_wlan_softmac::WlanSoftmacIfc> ifc_client) override;
  void WlantapMacStop() override;
  void WlantapMacQueueTx(const fuchsia_wlan_softmac::wire::WlanTxPacket& pkt) override;
  void WlantapMacSetChannel(const wlan_common::WlanChannel& channel) override;
  void WlantapMacJoinBss(const wlan_common::JoinBssRequest& config) override;
  void WlantapMacStartScan(uint64_t scan_id) override;
  void WlantapMacSetKey(const wlan_softmac::WlanKeyConfiguration& key_config) override;

 private:
  const std::shared_ptr<const wlan_tap::WlantapPhyConfig> phy_config_;
  std::string name_;
  fidl::ServerBindingRef<fuchsia_wlan_tap::WlantapPhy> user_binding_;
  size_t report_tx_status_count_ = 0;
  fdf::Logger* logger_;
  fdf::WireSharedClient<fuchsia_wlan_softmac::WlanSoftmacIfc> wlan_softmac_ifc_client_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> phy_controller_;
};

}  // namespace wlan

#endif  // SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_WLANTAP_PHY_H_
