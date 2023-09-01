// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "fullmac_mlme.h"

#include <wlan/common/logging.h>

#include "device.h"
#include "fuchsia/wlan/common/c/banjo.h"
#include "src/connectivity/wlan/lib/mlme/fullmac/c-binding/bindings.h"

namespace wlanif {

#define DEVICE(c) static_cast<Device *>(c)

FullmacMlme::FullmacMlme(Device *device)
    : device_(device), rust_mlme_(nullptr, delete_fullmac_mlme) {
  debugfn();
}

void FullmacMlme::Init() {
  debugfn();

  auto rust_device = rust_fullmac_device_interface_t{
      .device = static_cast<void *>(this->device_),
      .start = [](void *device, const rust_wlan_fullmac_ifc_protocol_copy_t *ifc,
                  zx_handle_t *out_sme_channel) -> zx_status_t {
        zx::channel channel;
        zx_status_t result = DEVICE(device)->Start(ifc, &channel);
        *out_sme_channel = channel.release();
        return result;
      },
      .query_device_info = [](void *device) -> wlan_fullmac_query_info_t {
        wlan_fullmac_query_info_t out_resp;
        DEVICE(device)->QueryDeviceInfo(&out_resp);
        return out_resp;
      },
      .query_mac_sublayer_support = [](void *device) -> mac_sublayer_support_t {
        mac_sublayer_support_t out_resp;
        DEVICE(device)->QueryMacSublayerSupport(&out_resp);
        return out_resp;
      },
      .query_security_support = [](void *device) -> security_support_t {
        security_support_t out_resp;
        DEVICE(device)->QuerySecuritySupport(&out_resp);
        return out_resp;
      },
      .query_spectrum_management_support = [](void *device) -> spectrum_management_support_t {
        spectrum_management_support_t out_resp;
        DEVICE(device)->QuerySpectrumManagementSupport(&out_resp);
        return out_resp;
      },
      .start_scan =
          [](void *device, wlan_fullmac_impl_start_scan_request_t *req) {
            DEVICE(device)->StartScan(req);
          },
      .connect = [](void *device,
                    wlan_fullmac_impl_connect_request_t *req) { DEVICE(device)->Connect(req); },
      .reconnect_req = [](void *device,
                          wlan_fullmac_reconnect_req_t *req) { DEVICE(device)->ReconnectReq(req); },
      .auth_resp = [](void *device,
                      wlan_fullmac_auth_resp_t *resp) { DEVICE(device)->AuthenticateResp(resp); },
      .deauth_req = [](void *device,
                       wlan_fullmac_deauth_req_t *req) { DEVICE(device)->DeauthenticateReq(req); },
      .assoc_resp = [](void *device,
                       wlan_fullmac_assoc_resp_t *resp) { DEVICE(device)->AssociateResp(resp); },
      .disassoc_req =
          [](void *device, wlan_fullmac_disassoc_req_t *req) {
            DEVICE(device)->DisassociateReq(req);
          },
      .reset_req = [](void *device,
                      wlan_fullmac_reset_req_t *req) { DEVICE(device)->ResetReq(req); },
      .start_req = [](void *device,
                      wlan_fullmac_start_req_t *req) { DEVICE(device)->StartReq(req); },
      .stop_req = [](void *device, wlan_fullmac_stop_req_t *req) { DEVICE(device)->StopReq(req); },
      .set_keys_req = [](void *device,
                         wlan_fullmac_set_keys_req_t *req) -> wlan_fullmac_set_keys_resp_t {
        wlan_fullmac_set_keys_resp_t out_resp;
        DEVICE(device)->SetKeysReq(req, &out_resp);
        return out_resp;
      },
      .del_keys_req = [](void *device,
                         wlan_fullmac_del_keys_req_t *req) { DEVICE(device)->DeleteKeysReq(req); },
      .eapol_req = [](void *device,
                      wlan_fullmac_eapol_req_t *req) { DEVICE(device)->EapolReq(req); },
      .get_iface_counter_stats = [](void *device,
                                    int32_t *out_status) -> wlan_fullmac_iface_counter_stats_t {
        wlan_fullmac_iface_counter_stats_t out_stats;
        *out_status = DEVICE(device)->GetIfaceCounterStats(&out_stats);
        return out_stats;
      },
      .get_iface_histogram_stats = [](void *device,
                                      int32_t *out_status) -> wlan_fullmac_iface_histogram_stats_t {
        wlan_fullmac_iface_histogram_stats_t out_stats;
        *out_status = DEVICE(device)->GetIfaceHistogramStats(&out_stats);
        return out_stats;
      },
      .sae_handshake_resp =
          [](void *device, wlan_fullmac_sae_handshake_resp_t *resp) {
            DEVICE(device)->SaeHandshakeResp(resp);
          },
      .sae_frame_tx = [](void *device,
                         wlan_fullmac_sae_frame_t *frame) { DEVICE(device)->SaeFrameTx(frame); },
      .wmm_status_req = [](void *device) { DEVICE(device)->WmmStatusReq(); },
      .on_link_state_changed = [](void *device,
                                  bool online) { DEVICE(device)->OnLinkStateChanged(online); },

  };
  rust_mlme_ = RustFullmacMlme(start_fullmac_mlme(rust_device), delete_fullmac_mlme);
}

void FullmacMlme::StopMainLoop() { stop_fullmac_mlme(rust_mlme_.get()); }

}  // namespace wlanif
