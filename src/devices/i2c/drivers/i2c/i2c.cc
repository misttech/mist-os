// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "i2c.h"

#include <fidl/fuchsia.hardware.i2cimpl/cpp/driver/wire.h>
#include <lib/ddk/binding_driver.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/metadata.h>
#include <lib/ddk/trace/event.h>
#include <lib/zx/profile.h>
#include <lib/zx/thread.h>

#include <memory>

#include <fbl/alloc_checker.h>

#include "i2c-child.h"

namespace i2c {

zx_status_t I2cDevice::Create(void* ctx, zx_device_t* parent) {
  zx::result i2c = DdkConnectRuntimeProtocol<fuchsia_hardware_i2cimpl::Service::Device>(parent);
  if (i2c.is_error()) {
    zxlogf(ERROR, "DdkConnectFidlProtocol() error: %s", zx_status_get_string(i2c.error_value()));
    return i2c.error_value();
  }

  auto decoded = ddk::GetEncodedMetadata<fuchsia_hardware_i2c_businfo::wire::I2CBusMetadata>(
      parent, DEVICE_METADATA_I2C_CHANNELS);
  if (!decoded.is_ok()) {
    zxlogf(ERROR, "Failed to decode metadata: %s", decoded.status_string());
    return decoded.error_value();
  }

  if (!decoded->has_channels()) {
    zxlogf(ERROR, "No channels supplied");
    return ZX_ERR_NO_RESOURCES;
  }

  fdf::Arena arena('I2CI');
  fdf::WireUnownedResult max_transfer_size =
      fdf::WireCall(i2c.value()).buffer(arena)->GetMaxTransferSize();
  if (!max_transfer_size.ok()) {
    zxlogf(ERROR, "Failed to send GetMaxTransferSize request: %s",
           max_transfer_size.status_string());
    return max_transfer_size.status();
  }
  if (max_transfer_size->is_error()) {
    zxlogf(ERROR, "Failed to get max transfer size: %s",
           zx_status_get_string(max_transfer_size->error_value()));
    return max_transfer_size->error_value();
  }

  fbl::AllocChecker ac;
  std::unique_ptr<I2cDevice> device(
      new (&ac) I2cDevice(parent, max_transfer_size->value()->size, std::move(i2c.value())));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }

  zx_status_t status = device->DdkAdd(ddk::DeviceAddArgs("i2c").set_flags(DEVICE_ADD_NON_BINDABLE));
  if (status != ZX_OK) {
    return status;
  }

  status = device->Init(**decoded);

  [[maybe_unused]] auto* unused = device.release();

  return status;
}

void I2cDevice::Transact(const uint16_t address, TransferRequestView request,
                         TransferCompleter::Sync& completer) {
  TRACE_DURATION("i2c", "I2cDevice Process Queued Transacts");

  SetSchedulerRole();

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
    zxlogf(ERROR, "Failed to send Transfer request: %s", result.status_string());
    completer.ReplyError(result.status());
    return;
  }
  if (result->is_error()) {
    // Don't log at ERROR severity here, as some I2C devices intentionally NACK to indicate that
    // they are busy.
    zxlogf(DEBUG, "Failed to perform transfer: %s", zx_status_get_string(result->error_value()));
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

zx_status_t I2cDevice::Init(const fuchsia_hardware_i2c_businfo::wire::I2CBusMetadata& metadata) {
  ZX_DEBUG_ASSERT(metadata.has_channels());

  zxlogf(DEBUG, "%zu channels supplied.", metadata.channels().count());

  const uint32_t bus_id = metadata.has_bus_id() ? metadata.bus_id() : 0;
  for (const auto& channel : metadata.channels()) {
    if (auto status = I2cChild::CreateAndAddDevice(bus_id, channel, this); status != ZX_OK) {
      return status;
    }
  }

  return ZX_OK;
}

void I2cDevice::SetSchedulerRole() {
  static std::once_flag profile_flag;
  std::call_once(profile_flag, [device = zxdev()] {
    // Set role for bus transaction thread.
    const char* role_name = "fuchsia.devices.i2c.drivers.i2c.bus";
    zx_status_t status =
        device_set_profile_by_role(device, zx::thread::self()->get(), role_name, strlen(role_name));
    if (status != ZX_OK) {
      zxlogf(WARNING, "Failed to apply role to dispatch thread: %s", zx_status_get_string(status));
    }
  });
}

zx_status_t I2cDevice::GrowContainersIfNeeded(
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

static constexpr zx_driver_ops_t driver_ops = []() {
  zx_driver_ops_t ops = {};
  ops.version = DRIVER_OPS_VERSION;
  ops.bind = I2cDevice::Create;
  return ops;
}();

}  // namespace i2c

ZIRCON_DRIVER(i2c, i2c::driver_ops, "zircon", "0.1");
