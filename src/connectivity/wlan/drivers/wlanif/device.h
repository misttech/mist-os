// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEVICE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEVICE_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <fidl/fuchsia.wlan.fullmac/cpp/driver/wire.h>
#include <fuchsia/wlan/fullmac/c/banjo.h>
#include <fuchsia/wlan/mlme/cpp/fidl.h>
#include <lib/ddk/driver.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/driver/outgoing/cpp/outgoing_directory.h>
#include <lib/fidl/cpp/binding.h>
#include <lib/sync/cpp/completion.h>
#include <lib/zx/result.h>
#include <zircon/types.h>

#include <memory>
#include <mutex>

#include <sdk/lib/driver/logging/cpp/logger.h>

#include "fuchsia/wlan/common/c/banjo.h"
#include "fullmac_mlme.h"

namespace fdf {
using namespace fuchsia_driver_framework;
}

namespace wlanif {

class Device final : public fdf::DriverBase,
                     public fidl::WireAsyncEventHandler<fdf::NodeController>,
                     public fdf::WireServer<fuchsia_wlan_fullmac::WlanFullmacImplIfc> {
 public:
  explicit Device(fdf::DriverStartArgs start_args,
                  fdf::UnownedSynchronizedDispatcher driver_dispatcher);
  ~Device();

  zx_status_t Bind();
  zx_status_t ConnectToWlanFullmacImpl();

  static constexpr const char* Name() { return "wlanif"; }
  zx::result<> Start() override;
  void PrepareStop(fdf::PrepareStopCompleter completer) override;

  void InitMlme();

  zx_status_t StartFullmac(const rust_wlan_fullmac_ifc_protocol_copy_t* ifc,
                           zx::channel* out_sme_channel);

  void OnUnbound(fidl::UnbindInfo info, fdf::ServerEnd<fuchsia_wlan_fullmac::WlanFullmacImplIfc>);
  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_driver_framework::NodeController> metadata) override;
  void StartScan(const wlan_fullmac_impl_base_start_scan_request_t* req);
  void Connect(const wlan_fullmac_impl_base_connect_request_t* req);
  void Reconnect(const wlan_fullmac_impl_base_reconnect_request_t* req);
  void AuthenticateResp(const wlan_fullmac_impl_base_auth_resp_request_t* resp);
  void Deauthenticate(const wlan_fullmac_impl_base_deauth_request_t* req);
  void AssociateResp(const wlan_fullmac_impl_base_assoc_resp_request_t* resp);
  void Disassociate(const wlan_fullmac_impl_base_disassoc_request_t* req);
  void Reset(const wlan_fullmac_impl_base_reset_request_t* req);
  void StartBss(const wlan_fullmac_impl_base_start_bss_request_t* req);
  void StopBss(const wlan_fullmac_impl_base_stop_bss_request_t* req);
  void SetKeysReq(const wlan_fullmac_set_keys_req_t* req, wlan_fullmac_set_keys_resp_t* out_resp);
  void DeleteKeysReq(const wlan_fullmac_del_keys_req_t* req);
  void EapolTx(const wlan_fullmac_impl_base_eapol_tx_request_t* req);
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
  void OnPmkAvailable(OnPmkAvailableRequestView request, fdf::Arena& arena,
                      OnPmkAvailableCompleter::Sync& completer) override;
  void SaeHandshakeInd(SaeHandshakeIndRequestView request, fdf::Arena& arena,
                       SaeHandshakeIndCompleter::Sync& completer) override;
  void SaeFrameRx(SaeFrameRxRequestView request, fdf::Arena& arena,
                  SaeFrameRxCompleter::Sync& completer) override;
  void OnWmmStatusResp(OnWmmStatusRespRequestView request, fdf::Arena& arena,
                       OnWmmStatusRespCompleter::Sync& completer) override;

 protected:
  void Shutdown();

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
  fdf::WireSharedClient<fuchsia_wlan_fullmac::WlanFullmacImpl> client_;

  // Dispatcher for making requests to the vendor driver over WlanFullmacImpl.
  fdf::Dispatcher client_dispatcher_;

  // Dispatcher for being a FIDL server firing replies to WlanIf device
  fdf::Dispatcher server_dispatcher_;

  fidl::WireClient<fdf::Node> parent_node_;
};

}  // namespace wlanif

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEVICE_H_
