// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/bin/driver_host/fidl_proxy_device.h"

#include <fbl/auto_lock.h>
#include <fbl/mutex.h>

#include "lib/fdio/directory.h"
#include "lib/zx/result.h"
#include "src/devices/bin/driver_host/driver_host_context.h"

namespace {

class FidlProxyDeviceInstance {
 public:
  FidlProxyDeviceInstance(zx_device_t* zxdev, fidl::ClientEnd<fuchsia_io::Directory> incoming_dir)
      : zxdev_(zxdev), incoming_dir_(std::move(incoming_dir)) {}

  static std::unique_ptr<FidlProxyDeviceInstance> Create(
      fbl::RefPtr<zx_device> zxdev, fidl::ClientEnd<fuchsia_io::Directory> incoming_dir) {
    // Leak a reference to the zxdev here.  It will be cleaned up by the
    // device_unbind_reply() in Unbind().
    return std::make_unique<FidlProxyDeviceInstance>(fbl::ExportToRawPtr(&zxdev),
                                                     std::move(incoming_dir));
  }

  zx::result<> ConnectToProtocol(const char* protocol, zx::channel request) {
    fbl::StringBuffer<fuchsia_io::wire::kMaxPath> path;
    path.Append("svc/");
    path.Append(protocol);
    return zx::make_result(
        fdio_service_connect_at(incoming_dir_.channel().get(), path.c_str(), request.release()));
  }

  zx::result<> ConnectToProtocol(const char* service, const char* protocol, zx::channel request) {
    fbl::StringBuffer<fuchsia_io::wire::kMaxPath> path;
    path.AppendPrintf("svc/%s/default/%s", service, protocol);
    return zx::make_result(
        fdio_service_connect_at(incoming_dir_.channel().get(), path.c_str(), request.release()));
  }

  void Release() { delete this; }

  void Unbind() { device_unbind_reply(zxdev_); }

 private:
  zx_device_t* zxdev_;
  fidl::ClientEnd<fuchsia_io::Directory> incoming_dir_;
};

}  // namespace

void InitializeFidlProxyDevice(const fbl::RefPtr<zx_device>& dev,
                               fidl::ClientEnd<fuchsia_io::Directory> incoming_dir) {
  static const zx_protocol_device_t fidl_proxy_device_ops = []() {
    zx_protocol_device_t ops = {};
    ops.unbind = [](void* ctx) { static_cast<FidlProxyDeviceInstance*>(ctx)->Unbind(); };
    ops.release = [](void* ctx) { static_cast<FidlProxyDeviceInstance*>(ctx)->Release(); };
    return ops;
  }();

  auto fidl_proxy = fbl::MakeRefCounted<FidlProxyDevice>(dev);

  auto new_device = FidlProxyDeviceInstance::Create(dev, std::move(incoming_dir));

  dev->set_fidl_proxy(fidl_proxy);
  dev->set_ops(&fidl_proxy_device_ops);
  dev->set_ctx(new_device.release());
  // Flag that when this is cleaned up, we should run its release hook.
  dev->set_flag(DEV_FLAG_ADDED);
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
  return static_cast<FidlProxyDeviceInstance*>(device_->ctx())
      ->ConnectToProtocol(protocol, std::move(request));
}

zx::result<> FidlProxyDevice::ConnectToProtocol(const char* service, const char* protocol,
                                                zx::channel request) {
  return static_cast<FidlProxyDeviceInstance*>(device_->ctx())
      ->ConnectToProtocol(service, protocol, std::move(request));
}
