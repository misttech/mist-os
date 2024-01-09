// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "util.h"

#include <fidl/fuchsia.hardware.bluetooth/cpp/wire.h>
#include <lib/fdio/directory.h>
#include <lib/zx/channel.h>

#include "src/connectivity/bluetooth/core/bt-host/common/log.h"

using namespace bt;

namespace bthost {

fuchsia::hardware::bluetooth::FullHciHandle CreateHciHandle(const std::string& device_path) {
  zx::channel client, server;
  zx_status_t status = zx::channel::create(0, &client, &server);
  if (status != ZX_OK) {
    bt_log(WARN, "bt-host", "Failed to open HCI device. Could not create fidl channel");
    return nullptr;
  }

  status = fdio_service_connect(device_path.c_str(), server.release());
  if (status != ZX_OK) {
    bt_log(WARN, "bt-host", "Failed to open HCI device. Could not connect to service directory");
    return nullptr;
  }

  fuchsia::hardware::bluetooth::FullHciHandle hci_handle(std::move(client));
  return hci_handle;
}

}  // namespace bthost
