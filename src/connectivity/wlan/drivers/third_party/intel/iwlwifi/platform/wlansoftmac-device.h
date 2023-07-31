// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_PLATFORM_WLANSOFTMAC_DEVICE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_PLATFORM_WLANSOFTMAC_DEVICE_H_

#include <fidl/fuchsia.wlan.softmac/cpp/driver/wire.h>
#include <lib/fdf/cpp/dispatcher.h>

#include <memory>

#include "banjo/ieee80211.h"

struct iwl_mvm_vif;
struct iwl_trans;

namespace wlan::iwlwifi {

class MvmSta;
class WlanSoftmacDevice;

class WlanSoftmacDevice : public fdf::WireServer<fuchsia_wlan_softmac::WlanSoftmac> {
 public:
  WlanSoftmacDevice(iwl_trans* drvdata, uint16_t iface_id, struct iwl_mvm_vif* mvmvif);
  ~WlanSoftmacDevice();

  // WlanSoftmac protocol implementation.
  void Query(fdf::Arena& arena, QueryCompleter::Sync& completer);
  void QueryDiscoverySupport(fdf::Arena& arena, QueryDiscoverySupportCompleter::Sync& completer);
  void QueryMacSublayerSupport(fdf::Arena& arena,
                               QueryMacSublayerSupportCompleter::Sync& completer);
  void QuerySecuritySupport(fdf::Arena& arena, QuerySecuritySupportCompleter::Sync& completer);
  void QuerySpectrumManagementSupport(fdf::Arena& arena,
                                      QuerySpectrumManagementSupportCompleter::Sync& completer);
  void Start(StartRequestView request, fdf::Arena& arena, StartCompleter::Sync& completer);
  void Stop(fdf::Arena& arena, StopCompleter::Sync& completer);
  void QueueTx(QueueTxRequestView request, fdf::Arena& arena, QueueTxCompleter::Sync& completer);
  void SetChannel(SetChannelRequestView request, fdf::Arena& arena,
                  SetChannelCompleter::Sync& completer);
  void JoinBss(JoinBssRequestView request, fdf::Arena& arena, JoinBssCompleter::Sync& completer);
  void EnableBeaconing(EnableBeaconingRequestView request, fdf::Arena& arena,
                       EnableBeaconingCompleter::Sync& completer);
  void DisableBeaconing(fdf::Arena& arena, DisableBeaconingCompleter::Sync& completer);
  void InstallKey(InstallKeyRequestView request, fdf::Arena& arena,
                  InstallKeyCompleter::Sync& completer);
  void NotifyAssociationComplete(NotifyAssociationCompleteRequestView request, fdf::Arena& arena,
                                 NotifyAssociationCompleteCompleter::Sync& completer);
  void ClearAssociation(ClearAssociationRequestView request, fdf::Arena& arena,
                        ClearAssociationCompleter::Sync& completer);
  void StartPassiveScan(StartPassiveScanRequestView request, fdf::Arena& arena,
                        StartPassiveScanCompleter::Sync& completer);
  void StartActiveScan(StartActiveScanRequestView request, fdf::Arena& arena,
                       StartActiveScanCompleter::Sync& completer);
  void CancelScan(CancelScanRequestView request, fdf::Arena& arena,
                  CancelScanCompleter::Sync& completer);
  void UpdateWmmParameters(UpdateWmmParametersRequestView request, fdf::Arena& arena,
                           UpdateWmmParametersCompleter::Sync& completer);

  // Entry functions to access WlanSoftmacIfc protocol implementation in client_.
  void Recv(fuchsia_wlan_softmac::wire::WlanRxPacket* rx_packet);
  void NotifyScanComplete(zx_status_t status, uint64_t scan_id);

  // Helper function
  bool IsValidChannel(const fuchsia_wlan_common::wire::WlanChannel* channel);

  void ServiceConnectHandler(fdf_dispatcher_t* dispatcher,
                             fdf::ServerEnd<fuchsia_wlan_softmac::WlanSoftmac> server_end);

 protected:
  struct iwl_mvm_vif* mvmvif_;

 private:
  iwl_trans* drvdata_;

  // Each peer on this interface will require a MvmSta instance.  For now, as we only support client
  // mode, we have only one peer (the AP), which simplifies things.
  std::unique_ptr<MvmSta> ap_mvm_sta_;

  // The FIDL client to communicate with Wlan device.
  fdf::WireSyncClient<fuchsia_wlan_softmac::WlanSoftmacIfc> client_;
};

}  // namespace wlan::iwlwifi

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_PLATFORM_WLANSOFTMAC_DEVICE_H_
