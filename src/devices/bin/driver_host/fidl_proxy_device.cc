// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_host/fidl_proxy_device.h"

#include <lib/fdio/directory.h>
#include <lib/zx/result.h>

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>

#include "src/devices/bin/driver_host/driver_host_context.h"

void InitializeFidlProxyDevice(const fbl::RefPtr<zx_device>& dev,
                               fidl::ClientEnd<fuchsia_io::Directory> incoming_dir) {
  static const zx_protocol_device_t empty_device_ops = {};

  fbl::RefPtr<FidlProxyDevice> fidl_proxy_device =
      fbl::MakeRefCounted<FidlProxyDevice>(std::move(incoming_dir));

  dev->set_fidl_proxy(fidl_proxy_device);

  // FIDL proxies aren't real devices, so they don't have a context object or any device operations.
  dev->set_ctx(nullptr);
  dev->set_ops(&empty_device_ops);
}

fbl::RefPtr<zx_driver> GetFidlProxyDriver(DriverHostContext* ctx) {
  static fbl::Mutex lock;
  static fbl::RefPtr<zx_driver> fidl_proxy_driver TA_GUARDED(lock);

  fbl::AutoLock guard(&lock);
  if (fidl_proxy_driver == nullptr) {
    auto status =
        zx_driver::Create("<internal:proxy>", ctx->inspect().drivers(), &fidl_proxy_driver);
    if (status != ZX_OK) {
      return nullptr;
    }
    fidl_proxy_driver->set_name("internal:proxy");
  }
  return fidl_proxy_driver;
}

zx::result<> FidlProxyDevice::ConnectToProtocol(const char* protocol, zx::channel request) {
  fbl::StringBuffer<fuchsia_io::wire::kMaxPathLength> path;
  path.AppendPrintf("svc/%s", protocol);
  return zx::make_result(
      fdio_service_connect_at(incoming_dir_.channel().get(), path.c_str(), request.release()));
}

zx::result<> FidlProxyDevice::ConnectToProtocol(const char* service, const char* protocol,
                                                zx::channel request) {
  fbl::StringBuffer<fuchsia_io::wire::kMaxPathLength> path;
  path.AppendPrintf("svc/%s/default/%s", service, protocol);
  return zx::make_result(
      fdio_service_connect_at(incoming_dir_.channel().get(), path.c_str(), request.release()));
}
