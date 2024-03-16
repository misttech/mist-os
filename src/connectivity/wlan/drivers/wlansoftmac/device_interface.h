// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_DEVICE_INTERFACE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_DEVICE_INTERFACE_H_

#include <fidl/fuchsia.wlan.softmac/cpp/fidl.h>
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
  static const DeviceInterface* from(const void* device) {
    return static_cast<const DeviceInterface*>(device);
  }

  virtual zx_status_t Start(zx_handle_t softmac_ifc_bridge_client_handle,
                            const frame_processor_t* frame_processor,
                            zx::channel* out_sme_channel) const = 0;

  virtual zx_status_t SetEthernetStatus(uint32_t status) const = 0;
};

}  // namespace wlan::drivers

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_DEVICE_INTERFACE_H_
