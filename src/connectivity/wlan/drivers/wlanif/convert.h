// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_CONVERT_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_CONVERT_H_

#include <fidl/fuchsia.wlan.fullmac/cpp/driver/wire.h>
#include <fuchsia/wlan/common/c/banjo.h>
#include <fuchsia/wlan/fullmac/c/banjo.h>
#include <fuchsia/wlan/internal/c/banjo.h>
#include <fuchsia/wlan/internal/cpp/fidl.h>
#include <fuchsia/wlan/mlme/cpp/fidl.h>
#include <net/ethernet.h>

namespace wlanif {

void ConvertCSsid(const cssid_t& cssid, fuchsia_wlan_ieee80211::wire::CSsid* out_cssid);

void ConvertScanReq(const wlan_fullmac_impl_start_scan_request_t& in,
                    fuchsia_wlan_fullmac::wire::WlanFullmacImplStartScanRequest* out,
                    fidl::AnyArena& arena);

void ConvertConnectReq(const wlan_fullmac_impl_connect_request_t& in,
                       fuchsia_wlan_fullmac::wire::WlanFullmacImplConnectRequest* out,
                       fidl::AnyArena& arena);

fuchsia_wlan_fullmac::wire::WlanAuthResult ConvertAuthResult(uint8_t in);

fuchsia_wlan_common::wire::WlanKeyType ConvertWlanKeyType(const wlan_key_type_t& in);
void ConvertSetKeyDescriptor(const set_key_descriptor_t& in,
                             fuchsia_wlan_fullmac::wire::SetKeyDescriptor* out,
                             fidl::AnyArena& arena);
void ConvertDeleteKeyDescriptor(const delete_key_descriptor_t& in,
                                fuchsia_wlan_fullmac::wire::DeleteKeyDescriptor* out);

fuchsia_wlan_common::wire::BssType ConvertBssType(const bss_type_t& in);

fuchsia_wlan_fullmac::wire::WlanAssocResult ConvertAssocResult(uint8_t code);

fuchsia_wlan_ieee80211::wire::ReasonCode ConvertReasonCode(uint16_t reason_code);

fuchsia_wlan_common::wire::WlanMacRole ConvertMacRole(wlan_mac_role_t role);
void ConvertBandCapability(const wlan_fullmac_band_capability_t& in,
                           fuchsia_wlan_fullmac::wire::WlanFullmacBandCapability* out);

void ConvertQueryInfo(fuchsia_wlan_fullmac::wire::WlanFullmacQueryInfo& in,
                      wlan_fullmac_query_info_t* out);

void ConvertMacSublayerSupport(const fuchsia_wlan_common::wire::MacSublayerSupport& in,
                               mac_sublayer_support_t* out);
void ConvertSecuritySupport(const fuchsia_wlan_common::wire::SecuritySupport& in,
                            security_support_t* out);
void ConvertSpectrumManagementSupport(
    const fuchsia_wlan_common::wire::SpectrumManagementSupport& in,
    spectrum_management_support_t* out);

void ConvertIfaceCounterStats(const fuchsia_wlan_fullmac::wire::WlanFullmacIfaceCounterStats& in,
                              wlan_fullmac_iface_counter_stats_t* out);

void ConvertNoiseFloorHistogram(
    const fuchsia_wlan_fullmac::wire::WlanFullmacNoiseFloorHistogram& in,
    wlan_fullmac_noise_floor_histogram_t* out);

void ConvertRxRateIndexHistogram(
    const fuchsia_wlan_fullmac::wire::WlanFullmacRxRateIndexHistogram& in,
    wlan_fullmac_rx_rate_index_histogram_t* out);

void ConvertRssiHistogram(const fuchsia_wlan_fullmac::wire::WlanFullmacRssiHistogram& in,
                          wlan_fullmac_rssi_histogram_t* out);

void ConvertSnrHistogram(const fuchsia_wlan_fullmac::wire::WlanFullmacSnrHistogram& in,
                         wlan_fullmac_snr_histogram_t* out);

void ConvertSaeHandshakeResp(const wlan_fullmac_sae_handshake_resp_t& in,
                             fuchsia_wlan_fullmac::wire::WlanFullmacSaeHandshakeResp* out);

void ConvertSaeFrame(const wlan_fullmac_sae_frame_t& in,
                     fuchsia_wlan_fullmac::wire::WlanFullmacSaeFrame* out, fidl::AnyArena& arena);

void ConvertBssDescription(const fuchsia_wlan_internal::wire::BssDescription& in,
                           bss_description_t* out);

void ConvertFullmacScanResult(const fuchsia_wlan_fullmac::wire::WlanFullmacScanResult& in,
                              wlan_fullmac_scan_result_t* out);

void ConvertScanEnd(const fuchsia_wlan_fullmac::wire::WlanFullmacScanEnd& in,
                    wlan_fullmac_scan_end_t* out);

void ConvertConnectConfirm(const fuchsia_wlan_fullmac::wire::WlanFullmacConnectConfirm& in,
                           wlan_fullmac_connect_confirm_t* out);

void ConvertRoamConfirm(const fuchsia_wlan_fullmac::wire::WlanFullmacRoamConfirm& in,
                        wlan_fullmac_roam_confirm_t* out);

void ConvertAuthInd(const fuchsia_wlan_fullmac::wire::WlanFullmacAuthInd& in,
                    wlan_fullmac_auth_ind_t* out);

void ConvertDeauthInd(const fuchsia_wlan_fullmac::wire::WlanFullmacDeauthIndication& in,
                      wlan_fullmac_deauth_indication_t* out);

void ConvertAssocInd(const fuchsia_wlan_fullmac::wire::WlanFullmacAssocInd& in,
                     wlan_fullmac_assoc_ind_t* out);

void ConvertDisassocInd(const fuchsia_wlan_fullmac::wire::WlanFullmacDisassocIndication& in,
                        wlan_fullmac_disassoc_indication_t* out);

uint8_t ConvertStartResultCode(fuchsia_wlan_fullmac::wire::WlanStartResult code);

uint8_t ConvertStopResultCode(fuchsia_wlan_fullmac::wire::WlanStopResult code);

void ConvertEapolConf(const fuchsia_wlan_fullmac::wire::WlanFullmacEapolConfirm& in,
                      wlan_fullmac_eapol_confirm_t* out);

void ConvertEapolIndication(const fuchsia_wlan_fullmac::wire::WlanFullmacEapolIndication& in,
                            wlan_fullmac_eapol_indication_t* out);

void ConvertSaeFrame(const fuchsia_wlan_fullmac::wire::WlanFullmacSaeFrame& in,
                     wlan_fullmac_sae_frame_t* out);

void ConvertWmmParams(const fuchsia_wlan_common::wire::WlanWmmParameters& in,
                      wlan_wmm_parameters_t* out);

}  // namespace wlanif

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANIF_CONVERT_H_
