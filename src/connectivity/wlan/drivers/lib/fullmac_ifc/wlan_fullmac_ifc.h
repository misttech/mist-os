// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_LIB_FULLMAC_IFC_WLAN_FULLMAC_IFC_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_LIB_FULLMAC_IFC_WLAN_FULLMAC_IFC_H_

#include <fidl/fuchsia.wlan.fullmac/cpp/driver/wire.h>

#include "fuchsia/hardware/wlan/fullmac/c/banjo.h"

/*
  This is a temporary library that converts banjo type to FIDL wire types and fire FIDL requests
  defined in WlanFullmacImplIfc protocol with the converted data. This library can be removed after
  all the WLAN vendor drivers become banjo-free.
*/
namespace wlan {

class WlanFullmacIfc {
 public:
  WlanFullmacIfc(fdf::ClientEnd<fuchsia_wlan_fullmac::WlanFullmacImplIfc> client_end);
  ~WlanFullmacIfc() = default;

  // WlanFullmacImplIfc implementations, will be invoked by brcmfmac.
  void OnScanResult(const wlan_fullmac_scan_result_t* res);
  void OnScanEnd(const wlan_fullmac_scan_end_t* end);
  void ConnectConf(const wlan_fullmac_connect_confirm_t* conf);
  void RoamConf(const wlan_fullmac_roam_confirm_t* conf);
  void AuthInd(const wlan_fullmac_auth_ind_t* ind);
  void DeauthConf(const wlan_fullmac_deauth_confirm_t* conf);
  void DeauthInd(const wlan_fullmac_deauth_indication_t* ind);
  void AssocInd(const wlan_fullmac_assoc_ind_t* ind);
  void DisassocConf(const wlan_fullmac_disassoc_confirm_t* conf);
  void DisassocInd(const wlan_fullmac_disassoc_indication_t* ind);
  void StartConf(const wlan_fullmac_start_confirm_t* conf);
  void StopConf(const wlan_fullmac_stop_confirm_t* conf);
  void EapolConf(const wlan_fullmac_eapol_confirm_t* conf);
  void OnChannelSwitch(const wlan_fullmac_channel_switch_info_t* info);
  void SignalReport(const wlan_fullmac_signal_report_indication* ind);
  void EapolInd(const wlan_fullmac_eapol_indication_t* ind);
  void RelayCapturedFrame(const wlan_fullmac_captured_frame_result_t* res);
  void OnPmkAvailable(const wlan_fullmac_pmk_info_t* info);
  void SaeHandshakeInd(const wlan_fullmac_sae_handshake_ind_t* ind);
  void SaeFrameRx(const wlan_fullmac_sae_frame_t* frame);
  void OnWmmStatusResp(zx_status_t status, const wlan_wmm_parameters_t* params);

 private:
  // The FIDL client to communicate with WLAN device
  fdf::WireSyncClient<fuchsia_wlan_fullmac::WlanFullmacImplIfc> ifc_client_;
};

}  // namespace wlan

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_LIB_FULLMAC_IFC_WLAN_FULLMAC_IFC_H_
