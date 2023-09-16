// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPAT_CPP_BANJO_SERVER_H_
#define LIB_DRIVER_COMPAT_CPP_BANJO_SERVER_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/compat/cpp/symbols.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/fit/function.h>

namespace compat {

class BanjoServer {
 public:
  BanjoServer(const char* name, uint32_t proto_id, void* ctx, void* ops) {
    // Set the symbols of the node that a driver will have access to.
    compat_device_.name = name;
    compat_device_.context = ctx;
    compat_device_.proto_ops.id = proto_id;
    compat_device_.proto_ops.ops = ops;
  }

  fuchsia_driver_framework::NodeSymbol symbol() const {
    return fuchsia_driver_framework::NodeSymbol{{
        .name = kDeviceSymbol,
        .address = reinterpret_cast<uint64_t>(&compat_device_),
    }};
  }

  fuchsia_driver_framework::NodeProperty property() const {
    return fdf::MakeProperty(1 /*BIND_PROTOCOL */, compat_device_.proto_ops.id);
  }

  DeviceServer::GetBanjoProtoCb callback() {
    return [this](uint32_t protocol) -> zx::result<DeviceServer::GenericProtocol> {
      if (protocol == compat_device_.proto_ops.id) {
        return zx::ok(proto());
      }
      return zx::error(ZX_ERR_NOT_SUPPORTED);
    };
  }

 private:
  DeviceServer::GenericProtocol proto() {
    return {.ops = const_cast<void*>(compat_device_.proto_ops.ops), .ctx = compat_device_.context};
  }

  device_t compat_device_ = kDefaultDevice;
};

}  // namespace compat

#endif  // LIB_DRIVER_COMPAT_CPP_BANJO_SERVER_H_
