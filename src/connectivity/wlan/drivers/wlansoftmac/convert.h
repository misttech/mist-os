// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_CONVERT_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_CONVERT_H_

#include <fidl/fuchsia.wlan.internal/cpp/fidl.h>
#include <fidl/fuchsia.wlan.softmac/cpp/driver/wire.h>
#include <fuchsia/wlan/common/c/banjo.h>
#include <fuchsia/wlan/internal/c/banjo.h>
#include <fuchsia/wlan/softmac/c/banjo.h>
#include <net/ethernet.h>

namespace wlan {

// FIDL to banjo conversions.
zx_status_t ConvertRxPacket(const fuchsia_wlan_softmac::wire::WlanRxPacket& in,
                            wlan_rx_packet_t* out, uint8_t* rx_packet_buffer);
zx_status_t ConvertTxStatus(const fuchsia_wlan_common::wire::WlanTxResult& in,
                            wlan_tx_result_t* out);

// banjo to FIDL conversions.
zx_status_t ConvertTxPacket(const uint8_t* data_in, size_t data_len_in,
                            const wlan_tx_info_t& info_in,
                            fuchsia_wlan_softmac::wire::WlanTxPacket* out);

}  // namespace wlan

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_CONVERT_H_
