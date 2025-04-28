// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "misc/drivers/mistos/device_server.h"

namespace mistos {

void DeviceServer::Initialize(std::optional<BanjoConfig> banjo_config) {
  // name_ = std::move(name);
  // service_offers_ = std::move(service_offers);
  banjo_config_ = std::move(banjo_config);
}

zx_status_t DeviceServer::GetProtocol(BanjoProtoId proto_id, GenericProtocol* out) const {
  if (!banjo_config_.has_value()) {
    return ZX_ERR_NOT_FOUND;
  }

  // If there is a specific entry for the proto_id, use it.
  auto specific_entry = banjo_config_->callbacks.find(proto_id);
  if (specific_entry != banjo_config_->callbacks.end()) {
    auto& get_banjo_protocol = specific_entry->second;
    if (out) {
      *out = get_banjo_protocol();
    }

    return ZX_OK;
  }

  // Otherwise use the generic one if one was provided.
  if (!banjo_config_->generic_callback) {
    return ZX_ERR_NOT_FOUND;
  }

  zx::result generic_result = banjo_config_->generic_callback(proto_id);
  if (generic_result.is_error()) {
    return ZX_ERR_NOT_FOUND;
  }

  if (out) {
    *out = generic_result.value();
  }

  return ZX_OK;
}

}  // namespace mistos
