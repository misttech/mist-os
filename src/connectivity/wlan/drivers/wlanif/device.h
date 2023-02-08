// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEVICE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEVICE_H_

#include <fuchsia/hardware/wlan/fullmac/c/banjo.h>
#include <fuchsia/wlan/mlme/cpp/fidl.h>
#include <lib/ddk/driver.h>
#include <lib/fidl/cpp/binding.h>

#include <memory>
#include <mutex>

#include "fuchsia/wlan/common/c/banjo.h"
#include "fullmac_mlme.h"

namespace wlanif {

class Device {
 public:
  Device(zx_device_t* device, wlan_fullmac_impl_protocol_t wlan_fullmac_impl_proto);
  ~Device();

  zx_status_t Bind();

  // zx_protocol_device_t
  void Unbind();
  void Release();

  zx_status_t Start(const rust_wlan_fullmac_ifc_protocol_copy_t* ifc, zx::channel* out_sme_channel);

  void StartScan(const wlan_fullmac_scan_req_t* req);
  void ConnectReq(const wlan_fullmac_connect_req_t* req);
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
  int32_t GetIfaceCounterStats(wlan_fullmac_iface_counter_stats_t* out_stats);
  int32_t GetIfaceHistogramStats(wlan_fullmac_iface_histogram_stats_t* out_stats);
  void SaeHandshakeResp(const wlan_fullmac_sae_handshake_resp_t* resp);
  void SaeFrameTx(const wlan_fullmac_sae_frame_t* frame);
  void WmmStatusReq();

  void OnLinkStateChanged(bool online);

 private:
  zx_status_t AddDevice();

  std::mutex lock_;
  std::mutex get_iface_histogram_stats_lock_;

  zx_device_t* parent_ = nullptr;
  zx_device_t* device_ = nullptr;

  wlan_fullmac_impl_protocol_t wlan_fullmac_impl_;

  // Manages the lifetime of the protocol struct we pass down to the vendor driver. Actual
  // calls to this protocol should only be performed by the vendor driver.
  std::unique_ptr<wlan_fullmac_impl_ifc_protocol_ops_t> wlan_fullmac_impl_ifc_ops_;
  std::unique_ptr<FullmacMlme> mlme_;

  bool device_online_ = false;
};

}  // namespace wlanif

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_DEVICE_H_
