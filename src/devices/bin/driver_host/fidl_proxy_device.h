// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_HOST_FIDL_PROXY_DEVICE_H_
#define SRC_DEVICES_BIN_DRIVER_HOST_FIDL_PROXY_DEVICE_H_

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/ddk/driver.h>
#include <lib/fidl/cpp/wire/channel.h>

#include "src/devices/bin/driver_host/zx_device.h"

// Modifies |device| to have the appropriate ctx and ops table for a FIDL proxy device.
void InitializeFidlProxyDevice(const fbl::RefPtr<zx_device>& device,
                               fidl::ClientEnd<fuchsia_io::Directory> incoming_dir);

// Returns a zx_driver instance for FIDL proxy devices.
fbl::RefPtr<zx_driver> GetFidlProxyDriver(DriverHostContext* ctx);

// |FidlProxyDevice| proxies the outgoing directory of a driver in a different driver host so that
// the driver's children can still access its FIDL protocols despite being in a different host. The
// FIDL proxy devices created in the driver manager correspond to |zx_device| objects with
// references to instances of this class.
class FidlProxyDevice : public fbl::RefCounted<FidlProxyDevice> {
 public:
  FidlProxyDevice(fidl::ClientEnd<fuchsia_io::Directory> incoming_dir)
      : incoming_dir_(std::move(incoming_dir)) {}
  ~FidlProxyDevice() = default;

  zx::result<> ConnectToProtocol(const char* protocol, zx::channel request);
  zx::result<> ConnectToProtocol(const char* service, const char* protocol, zx::channel request);

 private:
  fidl::ClientEnd<fuchsia_io::Directory> incoming_dir_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_HOST_FIDL_PROXY_DEVICE_H_
