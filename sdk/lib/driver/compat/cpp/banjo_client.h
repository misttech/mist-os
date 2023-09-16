// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.
#ifndef LIB_DRIVER_COMPAT_CPP_BANJO_CLIENT_H_
#define LIB_DRIVER_COMPAT_CPP_BANJO_CLIENT_H_

#include <fidl/fuchsia.driver.framework/cpp/fidl.h>
#include <lib/driver/compat/cpp/symbols.h>
#include <lib/driver/component/cpp/internal/symbols.h>

namespace compat {

template <typename Client>
zx::result<Client> ConnectBanjo(
    const std::optional<std::vector<fuchsia_driver_framework::NodeSymbol>>& symbols) {
  auto parent_symbol = fdf_internal::GetSymbol<device_t*>(symbols, kDeviceSymbol);

  typename Client::Proto proto = {};
  if (parent_symbol->proto_ops.id != Client::kProtocolId) {
    return zx::error(ZX_ERR_NOT_FOUND);
  }
  proto.ctx = parent_symbol->context;
  using OpsType = decltype(Client::Proto::ops);
  proto.ops = reinterpret_cast<OpsType>(parent_symbol->proto_ops.ops);

  Client client(&proto);
  if (!client.is_valid()) {
    return zx::error(ZX_ERR_INTERNAL);
  }
  return zx::ok(std::move(client));
}

}  // namespace compat

#endif  // LIB_DRIVER_COMPAT_CPP_BANJO_CLIENT_H_
