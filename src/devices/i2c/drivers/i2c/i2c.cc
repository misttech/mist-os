// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/i2c/drivers/i2c/i2c.h"

#include <fidl/fuchsia.hardware.i2c/cpp/fidl.h>
#include <fidl/fuchsia.hardware.i2cimpl/cpp/fidl.h>
#include <fidl/fuchsia.scheduler/cpp/fidl.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/metadata/cpp/metadata.h>
#include <lib/trace/event.h>

namespace i2c {

zx::result<> I2cDriver::Start() {
  auto i2cimpl_result = incoming()->Connect<fuchsia_hardware_i2cimpl::Service::Device>();
  if (i2cimpl_result.is_error()) {
    FDF_LOG(ERROR, "Failed to connect to fuchsia.hardware.i2cimpl service: %s",
            i2cimpl_result.status_string());
    return i2cimpl_result.take_error();
  }
  i2c_.Bind(std::move(*i2cimpl_result));

  zx::result result = incoming()->Connect<fuchsia_driver_compat::Service::Device>();
  if (result.is_error() || !result->is_valid()) {
    FDF_LOG(ERROR, "Failed to connect to compat service: %s", result.status_string());
    return result.take_error();
  }

  fidl::Arena arena;
  zx::result i2c_bus_metadata =
      fdf_metadata::GetMetadata<fuchsia_hardware_i2c_businfo::I2CBusMetadata>(*incoming());
  if (i2c_bus_metadata.is_error()) {
    FDF_LOG(ERROR, "Failed to get i2c_bus_metadata  %s", i2c_bus_metadata.status_string());
    return i2c_bus_metadata.take_error();
  }

  if (!i2c_bus_metadata->channels().has_value()) {
    FDF_LOG(ERROR, "No channels supplied from the metadata");
    return zx::error(ZX_ERR_NO_RESOURCES);
  }

  fdf::Arena i2c_arena('I2CI');
  fdf::WireUnownedResult max_transfer_size = i2c_.buffer(i2c_arena)->GetMaxTransferSize();
  if (!max_transfer_size.ok()) {
    FDF_LOG(ERROR, "Failed to send GetMaxTransferSize request: %s",
            max_transfer_size.status_string());
    return zx::error(max_transfer_size.status());
  }
  if (max_transfer_size->is_error()) {
    FDF_LOG(ERROR, "Failed to get max transfer size: %s",
            zx_status_get_string(max_transfer_size->error_value()));
    return zx::error(max_transfer_size->error_value());
  }
  max_transfer_ = max_transfer_size->value()->size;

  // Add owned i2c node.
  zx::result child = AddOwnedChild("i2c");
  if (child.is_error()) {
    FDF_LOG(ERROR, "failed to add i2c child node: %s", child.status_string());
    return child.take_error();
  }

  i2c_node_ = std::move(child->node_);
  return AddI2cChildren(i2c_bus_metadata.value());
}

zx::result<> I2cDriver::AddI2cChildren(
    const fuchsia_hardware_i2c_businfo::I2CBusMetadata& metadata) {
  if (!metadata.channels()) {
    FDF_LOG(ERROR, "Failed to find number of channels in metadata: %s",
            zx_status_get_string(ZX_ERR_NOT_FOUND));
    return zx::error(ZX_ERR_NOT_FOUND);
  }

  FDF_LOG(DEBUG, "Number of i2c channels supplied: %zu", metadata.channels()->size());
  const uint32_t bus_id = metadata.bus_id().value_or(0);
  for (const auto& channel : metadata.channels().value()) {
    // Add an i2c child to the owned i2c node.
    auto i2c_child_server = I2cChildServer::CreateAndAddChild(
        fit::bind_member(this, &I2cDriver::Transact), i2c_node_, logger(), bus_id, channel,
        incoming(), outgoing(), node_name());
    if (i2c_child_server.is_error()) {
      FDF_LOG(ERROR, "Failed to create child server: %s",
              zx_status_get_string(i2c_child_server.error_value()));
      return i2c_child_server.take_error();
    }
    child_servers_.push_back(std::move(i2c_child_server.value()));
  }

  return zx::ok();
}

void I2cDriver::Transact(uint16_t address, TransferRequestView request,
                         TransferCompleter::Sync& completer) {
  TRACE_DURATION("i2c", "I2cDevice Process Queued Transacts");

  const auto& transactions = request->transactions;
  if (zx_status_t status = GrowContainersIfNeeded(transactions); status != ZX_OK) {
    completer.ReplyError(status);
    return;
  }

  for (size_t i = 0; i < transactions.count(); ++i) {
    auto& impl_op = impl_ops_[i];
    const auto& transaction = transactions[i];

    // Same address for all ops, since there is one address per channel.
    impl_op.address = address;
    impl_op.stop = transaction.has_stop() && transaction.stop();

    auto& data_transfer = transaction.data_transfer();
    if (data_transfer.is_read_size()) {
      impl_op.type =
          fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithReadSize(data_transfer.read_size());

      if (impl_op.type.read_size() > max_transfer_) {
        completer.ReplyError(ZX_ERR_INVALID_ARGS);
        return;
      }
    } else {
      impl_op.type = fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithWriteData(
          fidl::ObjectView<fidl::VectorView<uint8_t>>::FromExternal(&data_transfer.write_data()));
      if (impl_op.type.write_data().empty()) {
        completer.ReplyError(ZX_ERR_INVALID_ARGS);
        return;
      }
    }
  }
  impl_ops_[transactions.count() - 1].stop = true;

  fdf::Arena arena('I2CI');
  fdf::WireUnownedResult result = i2c_.buffer(arena)->Transact(
      fidl::VectorView<fuchsia_hardware_i2cimpl::wire::I2cImplOp>::FromExternal(
          impl_ops_.data(), transactions.count()));
  if (!result.ok()) {
    FDF_LOG(ERROR, "Failed to send Transfer request: %s", result.status_string());
    completer.ReplyError(result.status());
    return;
  }
  if (result->is_error()) {
    // Don't log at ERROR severity here, as some I2C devices intentionally NACK to indicate that
    // they are busy.
    FDF_LOG(DEBUG, "Failed to perform transfer: %s", zx_status_get_string(result->error_value()));
    completer.ReplyError(result->error_value());
    return;
  }

  read_vectors_.clear();
  size_t read_buffer_offset = 0;
  for (const auto& read : result.value()->read) {
    auto dst = read_buffer_.data() + read_buffer_offset;
    auto len = read.data.count();
    memcpy(dst, read.data.data(), len);
    read_vectors_.emplace_back(fidl::VectorView<uint8_t>::FromExternal(dst, len));
    read_buffer_offset += len;
  }
  completer.ReplySuccess(fidl::VectorView<fidl::VectorView<uint8_t>>::FromExternal(read_vectors_));
}

zx_status_t I2cDriver::GrowContainersIfNeeded(
    const fidl::VectorView<fuchsia_hardware_i2c::wire::Transaction>& transactions) {
  if (transactions.count() < 1) {
    return ZX_ERR_INVALID_ARGS;
  }
  if (transactions.count() > fuchsia_hardware_i2c::wire::kMaxCountTransactions) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  size_t total_read_size = 0, total_write_size = 0;
  for (const auto transaction : transactions) {
    if (!transaction.has_data_transfer()) {
      return ZX_ERR_INVALID_ARGS;
    }

    if (transaction.data_transfer().is_write_data()) {
      total_write_size += transaction.data_transfer().write_data().count();
    } else if (transaction.data_transfer().is_read_size()) {
      total_read_size += transaction.data_transfer().read_size();
    } else {
      return ZX_ERR_INVALID_ARGS;
    }
  }

  if (total_read_size + total_write_size > fuchsia_hardware_i2c::wire::kMaxTransferSize) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  // Allocate space for all ops up front, if needed.
  if (transactions.count() > impl_ops_.size() || transactions.count() > read_vectors_.size()) {
    impl_ops_.resize(transactions.count());
    read_vectors_.resize(transactions.count());
  }
  if (total_read_size > read_buffer_.capacity()) {
    read_buffer_.resize(total_read_size);
  }

  return ZX_OK;
}

}  // namespace i2c

FUCHSIA_DRIVER_EXPORT(i2c::I2cDriver);
