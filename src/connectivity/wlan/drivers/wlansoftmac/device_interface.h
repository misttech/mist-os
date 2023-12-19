// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_DEVICE_INTERFACE_H_
#define SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_DEVICE_INTERFACE_H_

#include <fidl/fuchsia.wlan.softmac/cpp/fidl.h>
#include <fuchsia/wlan/common/c/banjo.h>
#include <lib/ddk/device.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <zircon/types.h>

#include <cstdint>
#include <cstring>

#include <fbl/ref_counted.h>
#include <wlan/common/macaddr.h>

#include "buffer_allocator.h"
#include "src/connectivity/wlan/drivers/wlansoftmac/rust_driver/c-binding/bindings.h"

namespace wlan::drivers {

class Packet;

// DeviceState represents the common runtime state of a device needed for
// interacting with external systems.
class DeviceState : public fbl::RefCounted<DeviceState> {
 public:
  bool online() const { return online_; }
  void set_online(bool online) { online_ = online; }

 private:
  bool online_ = false;
};

// DeviceInterface represents the actions that may interact with external
// systems.
class DeviceInterface {
 public:
  virtual ~DeviceInterface() = default;

  virtual zx_status_t Start(const rust_wlan_softmac_ifc_protocol_copy_t* ifc,
                            zx::channel* out_sme_channel) = 0;

  virtual zx_status_t DeliverEthernet(cpp20::span<const uint8_t> eth_frame) = 0;
  virtual zx_status_t QueueTx(UsedBuffer used_buffer, wlan_tx_info_t tx_info) = 0;

  virtual zx_status_t SetEthernetStatus(uint32_t status) = 0;
  virtual zx_status_t JoinBss(join_bss_request_t* cfg) = 0;
  virtual zx_status_t InstallKey(wlan_key_configuration_t* key_config) = 0;

  virtual fbl::RefPtr<DeviceState> GetState() = 0;
};

}  // namespace wlan::drivers

#endif  // SRC_CONNECTIVITY_WLAN_DRIVERS_WLANSOFTMAC_DEVICE_INTERFACE_H_
