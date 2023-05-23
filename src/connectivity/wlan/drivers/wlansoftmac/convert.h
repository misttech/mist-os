// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_CONVERT_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_CONVERT_H_

#include <fidl/fuchsia.wlan.softmac/cpp/driver/wire.h>
#include <fuchsia/wlan/common/c/banjo.h>
#include <fuchsia/wlan/internal/c/banjo.h>
#include <fuchsia/wlan/internal/cpp/fidl.h>
#include <fuchsia/wlan/mlme/cpp/fidl.h>
#include <fuchsia/wlan/softmac/c/banjo.h>
#include <net/ethernet.h>

namespace wlan {

// FIDL to banjo conversions.
zx_status_t ConvertWlanSoftmacQueryResponse(
    const fuchsia_wlan_softmac::wire::WlanSoftmacQueryResponse& in,
    wlan_softmac_query_response_t* out);
void ConvertDiscoverySuppport(const fuchsia_wlan_common::wire::DiscoverySupport& in,
                              discovery_support_t* out);
zx_status_t ConvertMacSublayerSupport(const fuchsia_wlan_common::wire::MacSublayerSupport& in,
                                      mac_sublayer_support_t* out);
void ConvertSecuritySupport(const fuchsia_wlan_common::wire::SecuritySupport& in,
                            security_support_t* out);
void ConvertSpectrumManagementSupport(
    const fuchsia_wlan_common::wire::SpectrumManagementSupport& in,
    spectrum_management_support_t* out);
zx_status_t ConvertRxPacket(const fuchsia_wlan_softmac::wire::WlanRxPacket& in,
                            wlan_rx_packet_t* out, uint8_t* rx_packet_buffer);
zx_status_t ConvertTxStatus(const fuchsia_wlan_common::wire::WlanTxResult& in,
                            wlan_tx_result_t* out);

// banjo to FIDL conversions.
zx_status_t ConvertMacRole(const wlan_mac_role_t& in, fuchsia_wlan_common::wire::WlanMacRole* out);
zx_status_t ConvertTxPacket(const uint8_t* data_in, const size_t data_len_in,
                            const wlan_tx_info_t& info_in,
                            fuchsia_wlan_softmac::wire::WlanTxPacket* out);
zx_status_t ConvertChannel(const wlan_channel_t& in, fuchsia_wlan_common::wire::WlanChannel* out);
zx_status_t ConvertJoinBssRequest(const join_bss_request_t& in,
                                  fuchsia_wlan_common::wire::JoinBssRequest* out,
                                  fidl::AnyArena& arena);
void ConvertEnableBeaconing(const wlan_softmac_enable_beaconing_request_t& in,
                            fuchsia_wlan_softmac::wire::WlanSoftmacEnableBeaconingRequest* out,
                            fidl::AnyArena& arena);
zx_status_t ConvertKeyConfig(const wlan_key_configuration_t& in,
                             fuchsia_wlan_softmac::wire::WlanKeyConfiguration* out,
                             fidl::AnyArena& arena);
void ConvertPassiveScanArgs(const wlan_softmac_start_passive_scan_request_t& in,
                            fuchsia_wlan_softmac::wire::WlanSoftmacStartPassiveScanRequest* out,
                            fidl::AnyArena& arena);
void ConvertActiveScanArgs(const wlan_softmac_start_active_scan_request_t& in,
                           fuchsia_wlan_softmac::wire::WlanSoftmacStartActiveScanRequest* out,
                           fidl::AnyArena& arena);

}  // namespace wlan

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_CONVERT_H_
