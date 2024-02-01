// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_DEVICE_INTERFACE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_DEVICE_INTERFACE_H_

#include <fidl/fuchsia.wlan.softmac/cpp/fidl.h>
#include <fuchsia/wlan/common/c/banjo.h>
#include <lib/ddk/device.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/trace/event.h>
#include <zircon/types.h>

#include <cstdint>
#include <cstring>

#include <fbl/ref_counted.h>
#include <wlan/common/macaddr.h>

#include "buffer_allocator.h"
#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers {

// DeviceInterface represents the actions that may interact with external
// systems.
class DeviceInterface {
 public:
  virtual ~DeviceInterface() = default;
  static DeviceInterface* from(void* device) { return static_cast<DeviceInterface*>(device); }

  virtual zx_status_t Start(const rust_wlan_softmac_ifc_protocol_copy_t* ifc,
                            zx_handle_t softmac_ifc_bridge_client_handle,
                            zx::channel* out_sme_channel) = 0;

  virtual zx_status_t DeliverEthernet(cpp20::span<const uint8_t> eth_frame) = 0;
  virtual zx_status_t QueueTx(UsedBuffer used_buffer, wlan_tx_info_t tx_info,
                              trace_async_id_t async_id) = 0;

  virtual zx_status_t SetEthernetStatus(uint32_t status) = 0;
};

}  // namespace wlan::drivers

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_DEVICE_INTERFACE_H_
