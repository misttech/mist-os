// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "examples/drivers/transport/zircon/v2/child-driver.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace zircon_transport {

zx::result<> ChildZirconTransportDriver::Start() {
  zx::result connect_result = incoming()->Connect<fuchsia_hardware_i2c::Service::Device>();
  if (connect_result.is_error() || !connect_result->is_valid()) {
    FDF_LOG(ERROR, "Failed to connect to fuchsia.hardware.i2c service: %s",
            connect_result.status_string());
    return connect_result.take_error();
  }

  auto result = QueryParent(std::move(connect_result.value()));
  if (result.is_error()) {
    return result.take_error();
  }

  zx::result child_result = AddChild("transport-child", {}, {});
  if (child_result.is_error()) {
    return child_result.take_error();
  }

  controller_.Bind(std::move(child_result.value()), dispatcher());
  return zx::ok();
}

zx::result<> ChildZirconTransportDriver::QueryParent(
    fidl::ClientEnd<fuchsia_hardware_i2c::Device> client_end) {
  // Query and store the i2c name.
  auto name_result = fidl::WireCall(client_end)->GetName();
  if (!name_result.ok()) {
    FDF_SLOG(ERROR, "Failed to request name.", KV("status", name_result.status_string()));
    return zx::error(name_result.status());
  }
  if (name_result->is_error()) {
    FDF_SLOG(ERROR, "Name request returned an error.", KV("status", name_result->error_value()));
    return name_result->take_error();
  }

  name_ = std::string(name_result.value()->name.get());
  FDF_LOG(INFO, "I2C name: %s", name_.c_str());

  // Transfer and read from the i2c server.
  fidl::Arena arena;
  auto i2c_transactions = fidl::VectorView<fuchsia_hardware_i2c::wire::Transaction>(arena, 1);
  i2c_transactions[0] =
      fuchsia_hardware_i2c::wire::Transaction::Builder(arena)
          .data_transfer(fuchsia_hardware_i2c::wire::DataTransfer::WithReadSize(3))
          .Build();

  auto transfer_result = fidl::WireCall(client_end)->Transfer(i2c_transactions);
  if (!transfer_result.ok()) {
    FDF_SLOG(ERROR, "Failed to request transfer.", KV("status", transfer_result.status_string()));
    return zx::error(transfer_result.status());
  }
  if (transfer_result->is_error()) {
    FDF_SLOG(ERROR, "Transfer returned an error.", KV("status", transfer_result->error_value()));
    return transfer_result->take_error();
  }

  read_result_.reserve(transfer_result->value()->read_data.count());
  for (auto& read_data : transfer_result->value()->read_data) {
    read_result_.emplace_back(std::vector<uint8_t>(read_data.begin(), read_data.end()));
  }
  return zx::ok();
}

}  // namespace zircon_transport

FUCHSIA_DRIVER_EXPORT(zircon_transport::ChildZirconTransportDriver);
