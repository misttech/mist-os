// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_WLANTAP_MAC_H_
#define SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_WLANTAP_MAC_H_

#include <fidl/fuchsia.wlan.softmac/cpp/driver/wire.h>
#include <fidl/fuchsia.wlan.tap/cpp/driver/wire.h>
#include <lib/ddk/device.h>

namespace wlan_tap = fuchsia_wlan_tap::wire;
namespace wlan_common = fuchsia_wlan_common::wire;
namespace wlan_softmac = fuchsia_wlan_softmac::wire;

namespace wlan {

class WlantapMac {
 public:
  using Ptr = std::unique_ptr<WlantapMac, std::function<void(WlantapMac*)>>;

  class Listener {
   public:
    virtual void WlantapMacStart() = 0;
    virtual void WlantapMacStop() = 0;
    virtual void WlantapMacQueueTx(const wlan_softmac::WlanTxPacket& pkt) = 0;
    virtual void WlantapMacSetChannel(const wlan_common::WlanChannel& channel) = 0;
    virtual void WlantapMacJoinBss(const wlan_common::JoinBssRequest& join_request) = 0;
    virtual void WlantapMacStartScan(uint64_t scan_id) = 0;
    virtual void WlantapMacSetKey(const wlan_softmac::WlanKeyConfiguration& key_config) = 0;
  };

  virtual void Rx(const fidl::VectorView<uint8_t>& data, const wlan_tap::WlanRxInfo& rx_info) = 0;

  virtual void ReportTxResult(const wlan_common::WlanTxResult& tr) = 0;

  virtual void ScanComplete(uint64_t scan_id, int32_t status) = 0;
  virtual void RemoveDevice() = 0;

  virtual ~WlantapMac() = default;
};

// If successful, this returns a pointer to WlantapMac that schedules its removal through the driver
// framework on destruction.
zx::result<WlantapMac::Ptr> CreateWlantapMac(
    zx_device_t* parent_phy, wlan_common::WlanMacRole role,
    std::shared_ptr<const wlan_tap::WlantapPhyConfig> phy_config, WlantapMac::Listener* listener,
    zx::channel sme_channel);

}  // namespace wlan

#endif  // SRC_CONNECTIVITY_WLAN_TESTING_WLANTAP_DRIVER_WLANTAP_MAC_H_
