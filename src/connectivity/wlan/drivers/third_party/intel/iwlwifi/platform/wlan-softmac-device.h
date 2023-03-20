// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_PLATFORM_WLAN_SOFTMAC_DEVICE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_PLATFORM_WLAN_SOFTMAC_DEVICE_H_

#include <fidl/fuchsia.wlan.softmac/cpp/driver/wire.h>
#include <lib/ddk/device.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/fdf/cpp/dispatcher.h>

#include <memory>

#include <ddktl/device.h>

#include "banjo/ieee80211.h"

struct iwl_mvm_vif;
struct iwl_trans;

namespace wlan::iwlwifi {

class MvmSta;
class WlanSoftmacDevice;

class WlanSoftmacDevice
    : public ddk::Device<WlanSoftmacDevice, ddk::Initializable, ddk::Unbindable>,
      public fdf::WireServer<fuchsia_wlan_softmac::WlanSoftmac> {
 public:
  WlanSoftmacDevice(zx_device* parent, iwl_trans* drvdata, uint16_t iface_id,
                    struct iwl_mvm_vif* mvmvif);
  ~WlanSoftmacDevice();

  void DdkInit(ddk::InitTxn txn);
  void DdkRelease();
  void DdkUnbind(ddk::UnbindTxn txn);

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
  void ConfigureBeaconing(ConfigureBeaconingRequestView request, fdf::Arena& arena,
                          ConfigureBeaconingCompleter::Sync& completer);
  void InstallKey(InstallKeyRequestView request, fdf::Arena& arena,
                  InstallKeyCompleter::Sync& completer);
  void ConfigureAssoc(ConfigureAssocRequestView request, fdf::Arena& arena,
                      ConfigureAssocCompleter::Sync& completer);
  void ClearAssoc(ClearAssocRequestView request, fdf::Arena& arena,
                  ClearAssocCompleter::Sync& completer);
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
  void ScanComplete(zx_status_t status, uint64_t scan_id);

  // Serves the WlanSoftmac protocol on `server_end`.
  zx_status_t ServeWlanSoftmacProtocol(fidl::ServerEnd<fuchsia_io::Directory> server_end);

  // Helper function
  bool IsValidChannel(const fuchsia_wlan_common::wire::WlanChannel* channel);

 protected:
  struct iwl_mvm_vif* mvmvif_;

 private:
  iwl_trans* drvdata_;
  uint16_t iface_id_;

  // True if the mac_start() has been executed successfully.
  bool mac_started;

  // Each peer on this interface will require a MvmSta instance.  For now, as we only support client
  // mode, we have only one peer (the AP), which simplifies things.
  std::unique_ptr<MvmSta> ap_mvm_sta_;

  // The FIDL client to communicate with Wlan device.
  fdf::WireSyncClient<fuchsia_wlan_softmac::WlanSoftmacIfc> client_;

  // Serves fuchsia_wlan_softmac::Service.
  fdf::OutgoingDirectory outgoing_dir_;

  bool serving_wlan_softmac_instance_;
};

}  // namespace wlan::iwlwifi

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_THIRD_PARTY_INTEL_IWLWIFI_PLATFORM_WLAN_SOFTMAC_DEVICE_H_
