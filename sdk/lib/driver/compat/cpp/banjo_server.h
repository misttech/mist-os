// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPAT_CPP_BANJO_SERVER_H_
#define LIB_DRIVER_COMPAT_CPP_BANJO_SERVER_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <lib/driver/compat/cpp/device_server.h>
#include <lib/driver/component/cpp/node_add_args.h>
#include <lib/fit/function.h>

namespace compat {

class BanjoServer final {
 public:
  BanjoServer(uint32_t proto_id, void* ctx, void* ops)
      : proto_id_(proto_id), ctx_(ctx), ops_(ops) {}

  fuchsia_driver_framework::NodeProperty property() const {
    return fdf::MakeProperty(1 /*BIND_PROTOCOL */, proto_id_);
  }

  DeviceServer::SpecificGetBanjoProtoCb callback() {
    return [this]() { return DeviceServer::GenericProtocol{.ops = ops_, .ctx = ctx_}; };
  }

 private:
  uint32_t proto_id_;
  void* ctx_;
  void* ops_;
};

}  // namespace compat

#endif  // LIB_DRIVER_COMPAT_CPP_BANJO_SERVER_H_
