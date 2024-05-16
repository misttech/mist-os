// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "util.h"

#include <lib/fdio/directory.h>
#include <lib/zx/channel.h>

#include "src/connectivity/bluetooth/core/bt-host/public/pw_bluetooth_sapphire/internal/host/common/log.h"

using namespace bt;

namespace bthost {

zx::result<fidl::ClientEnd<fuchsia_hardware_bluetooth::Vendor>> CreateVendorHandle(
    const std::string& device_path) {
  zx::channel client, server;
  zx_status_t status = zx::channel::create(0, &client, &server);
  if (status != ZX_OK) {
    bt_log(WARN, "bt-host", "Failed to open HCI device: Could not create FIDL channel");
    return zx::error(status);
  }

  status = fdio_service_connect(device_path.c_str(), server.release());
  if (status != ZX_OK) {
    bt_log(WARN, "bt-host", "Failed to open HCI device: Could not connect to service directory");
    return zx::error(status);
  }

  return zx::ok(fidl::ClientEnd<fuchsia_hardware_bluetooth::Vendor>(std::move(client)));
}

}  // namespace bthost
