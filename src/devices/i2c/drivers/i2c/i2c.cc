// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "i2c.h"

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
  ddk::I2cImplProtocolClient i2c(parent);
  if (!i2c.is_valid()) {
    zxlogf(ERROR, "Failed to get i2cimpl client");
    return ZX_ERR_NO_RESOURCES;
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

  uint64_t max_transfer_size = 0;
  if (zx_status_t status = i2c.GetMaxTransferSize(&max_transfer_size); status != ZX_OK) {
    zxlogf(ERROR, "Failed to get max transfer size: %s", zx_status_get_string(status));
    return status;
  }

  fbl::AllocChecker ac;
  std::unique_ptr<I2cDevice> device(new (&ac) I2cDevice(parent, max_transfer_size, i2c));
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

  uint8_t* read_buffer = read_buffer_.data();
  size_t read_ops = 0;

  for (size_t i = 0; i < transactions.count(); ++i) {
    // Same address for all ops, since there is one address per channel.
    impl_ops_[i].address = address;
    impl_ops_[i].stop = transactions[i].has_stop() && transactions[i].stop();
    impl_ops_[i].is_read = transactions[i].data_transfer().is_read_size();

    if (impl_ops_[i].is_read) {
      impl_ops_[i].data_buffer = read_buffer;
      impl_ops_[i].data_size = transactions[i].data_transfer().read_size();

      // Create a VectorView pointing to the read buffer ahead of time so that we don't have to loop
      // again to create the reply vector.
      read_vectors_[read_ops++] =
          fidl::VectorView<uint8_t>::FromExternal(impl_ops_[i].data_buffer, impl_ops_[i].data_size);
      read_buffer += impl_ops_[i].data_size;
    } else {
      impl_ops_[i].data_buffer = transactions[i].data_transfer().write_data().data();
      impl_ops_[i].data_size = transactions[i].data_transfer().write_data().count();
    }

    if (impl_ops_[i].data_size == 0 || impl_ops_[i].data_size > max_transfer_) {
      completer.ReplyError(ZX_ERR_INVALID_ARGS);
      return;
    }
  }
  impl_ops_[transactions.count() - 1].stop = true;

  zx_status_t status = i2c_.Transact(impl_ops_.data(), transactions.count());
  if (status == ZX_OK) {
    completer.ReplySuccess(
        fidl::VectorView<fidl::VectorView<uint8_t>>::FromExternal(read_vectors_.data(), read_ops));
  } else {
    completer.ReplyError(status);
  }
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
