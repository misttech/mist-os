// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_HOST_FIDL_PROXY_DEVICE_H_
#define SRC_DEVICES_BIN_DRIVER_HOST_FIDL_PROXY_DEVICE_H_

#include <fidl/fuchsia.io/cpp/wire.h>
#include <lib/ddk/driver.h>
#include <lib/fidl/cpp/wire/channel.h>

#include "src/devices/bin/driver_host/zx_device.h"

// Modifies |device| to have the appropriate protocol_id, ctx, and ops tables
// for a FIDL proxy device.
void InitializeFidlProxyDevice(const fbl::RefPtr<zx_device>& device,
                               fidl::ClientEnd<fuchsia_io::Directory> incoming_dir);

// Returns a zx_driver instance for FIDL proxy devices.
fbl::RefPtr<zx_driver> GetFidlProxyDriver(DriverHostContext* ctx);

class FidlProxyDevice : public fbl::RefCounted<FidlProxyDevice> {
 public:
  explicit FidlProxyDevice(fbl::RefPtr<zx_device> device) : device_(std::move(device)) {}
  ~FidlProxyDevice() = default;

  zx::result<> ConnectToProtocol(const char* protocol, zx::channel request);
  zx::result<> ConnectToProtocol(const char* service_name, const char* protocol,
                                 zx::channel request);

 private:
  fbl::RefPtr<zx_device> device_;
};

#endif  // SRC_DEVICES_BIN_DRIVER_HOST_FIDL_PROXY_DEVICE_H_
