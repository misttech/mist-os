// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_THIRD_PARTY_LWIP_INCLUDE_LWIP_MISTOS_INFERFACES_ADMIN_H_
#define VENDOR_MISTTECH_THIRD_PARTY_LWIP_INCLUDE_LWIP_MISTOS_INFERFACES_ADMIN_H_

#include <fuchsia/hardware/network/driver/cpp/banjo.h>

#include <lwip/netif.h>

#include "vendor/misttech/src/connectivity/lib/network-device/cpp/network_device_client.h"

namespace lwip::mistos {

struct State {
  port_id_t port_id;
  network::client::NetworkDeviceClient* client;
};

err_t CreateInterface(struct netif* netif);

}  // namespace lwip::mistos

#endif  // VENDOR_MISTTECH_THIRD_PARTY_LWIP_INCLUDE_LWIP_MISTOS_INFERFACES_ADMIN_H_
