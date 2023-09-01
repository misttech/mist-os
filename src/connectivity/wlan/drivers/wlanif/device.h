// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEVICE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEVICE_H_

#include <fidl/fuchsia.wlan.fullmac/cpp/driver/wire.h>
#include <fuchsia/wlan/fullmac/c/banjo.h>
#include <fuchsia/wlan/mlme/cpp/fidl.h>
#include <lib/ddk/driver.h>
#include <lib/fidl/cpp/binding.h>

#include <memory>
#include <mutex>

#include <ddktl/device.h>

#include "fuchsia/wlan/common/c/banjo.h"
#include "fullmac_mlme.h"

namespace wlanif {

class Device : public ddk::Device<Device, ddk::Unbindable>,
               public fdf::WireServer<fuchsia_wlan_fullmac::WlanFullmacImplIfc> {
 public:
  Device(zx_device_t* device);
  // Reserve this version of constructor for testing purpose.
  Device(zx_device_t* device, fdf::ClientEnd<fuchsia_wlan_fullmac::WlanFullmacImpl> client);
  ~Device();

  zx_status_t Bind();
  zx_status_t ConnectToWlanFullmacImpl();

  // Ddktl protocol implementations.
  void DdkUnbind(::ddk::UnbindTxn txn);
  void DdkRelease();

  void InitMlme();
  zx_status_t AddDevice();

  zx_status_t Start(const rust_wlan_fullmac_ifc_protocol_copy_t* ifc, zx::channel* out_sme_channel);

  void StartScan(const wlan_fullmac_impl_start_scan_request_t* req);
  void Connect(const wlan_fullmac_impl_connect_request_t* req);
  void ReconnectReq(const wlan_fullmac_reconnect_req_t* req);
  void AuthenticateResp(const wlan_fullmac_auth_resp_t* resp);
  void DeauthenticateReq(const wlan_fullmac_deauth_req_t* req);
  void AssociateResp(const wlan_fullmac_assoc_resp_t* resp);
  void DisassociateReq(const wlan_fullmac_disassoc_req_t* req);
  void ResetReq(const wlan_fullmac_reset_req_t* req);
  void StartReq(const wlan_fullmac_start_req_t* req);
  void StopReq(const wlan_fullmac_stop_req_t* req);
  void SetKeysReq(const wlan_fullmac_set_keys_req_t* req, wlan_fullmac_set_keys_resp_t* out_resp);
  void DeleteKeysReq(const wlan_fullmac_del_keys_req_t* req);
  void EapolReq(const wlan_fullmac_eapol_req_t* req);
  void QueryDeviceInfo(wlan_fullmac_query_info_t* out_resp);
  void QueryMacSublayerSupport(mac_sublayer_support_t* out_resp);
  void QuerySecuritySupport(security_support_t* out_resp);
  void QuerySpectrumManagementSupport(spectrum_management_support_t* out_resp);
  zx_status_t GetIfaceCounterStats(wlan_fullmac_iface_counter_stats_t* out_stats);
  zx_status_t GetIfaceHistogramStats(wlan_fullmac_iface_histogram_stats_t* out_stats);
  void SaeHandshakeResp(const wlan_fullmac_sae_handshake_resp_t* resp);
  void SaeFrameTx(const wlan_fullmac_sae_frame_t* frame);
  void WmmStatusReq();

  void OnLinkStateChanged(bool online);

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

 private:
  // Storage of histogram data.
  wlan_fullmac_hist_bucket_t
      noise_floor_buckets_[fuchsia_wlan_fullmac::wire::kWlanFullmacMaxNoiseFloorSamples];
  wlan_fullmac_hist_bucket_t rssi_buckets_[fuchsia_wlan_fullmac::wire::kWlanFullmacMaxRssiSamples];
  wlan_fullmac_hist_bucket_t
      rx_rate_index_buckets_[fuchsia_wlan_fullmac::wire::kWlanFullmacMaxRxRateIndexSamples];
  wlan_fullmac_hist_bucket_t snr_buckets_[fuchsia_wlan_fullmac::wire::kWlanFullmacMaxSnrSamples];
  wlan_fullmac_noise_floor_histogram_t noise_floor_histograms_;
  wlan_fullmac_rssi_histogram_t rssi_histograms_;
  wlan_fullmac_rx_rate_index_histogram_t rx_rate_index_histograms_;
  wlan_fullmac_snr_histogram_t snr_histograms_;

  std::mutex lock_;
  std::mutex get_iface_histogram_stats_lock_;

  // Manages the lifetime of the protocol struct we pass down to the vendor driver. Actual
  // calls to this protocol should only be performed by the vendor driver.
  std::unique_ptr<wlan_fullmac_impl_ifc_protocol_ops_t> wlan_fullmac_impl_ifc_protocol_ops_;
  std::unique_ptr<wlan_fullmac_impl_ifc_protocol_t> wlan_fullmac_impl_ifc_protocol_;
  std::unique_ptr<FullmacMlme> mlme_;

  bool device_online_ = false;
  // The FIDL Client to communicate with WlanIf device
  fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImpl> client_;

  // Dispatcher for being a FIDL server firing replies to WlanIf device
  fdf::Dispatcher server_dispatcher_;

  std::optional<::ddk::UnbindTxn> unbind_txn_;
};

}  // namespace wlanif

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEVICE_H_
