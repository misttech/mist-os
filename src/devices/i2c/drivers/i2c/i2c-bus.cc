// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "i2c-bus.h"

#include <lib/async/cpp/task.h>
#include <lib/ddk/debug.h>
#include <lib/ddk/trace/event.h>

#include <fbl/alloc_checker.h>

#include "i2c-child.h"

namespace i2c {

zx_status_t I2cBus::CreateAndAddChildren(
    zx_device_t* parent, const ddk::I2cImplProtocolClient& i2c, const uint32_t bus_id,
    const fidl::VectorView<fuchsia_hardware_i2c_businfo::wire::I2CChannel>& channels,
    async_dispatcher_t* const dispatcher) {
  uint64_t max_transfer_size = 0;
  if (zx_status_t status = i2c.GetMaxTransferSize(bus_id, &max_transfer_size); status != ZX_OK) {
    zxlogf(ERROR, "Failed to get max transfer size: %s", zx_status_get_string(status));
    return status;
  }

  return async::PostTask(dispatcher, [=, &channels]() {
    CreateAndAddChildrenOnDispatcher(parent, i2c, bus_id, max_transfer_size, channels, dispatcher);
  });
}

void I2cBus::Transact(const uint16_t address, TransferRequestView request,
                      TransferCompleter::Sync& completer) {
  TRACE_DURATION("i2c", "I2cBus Process Queued Transacts");

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

  zx_status_t status = i2c_.Transact(bus_id_, impl_ops_.data(), transactions.count());
  if (status == ZX_OK) {
    completer.ReplySuccess(
        fidl::VectorView<fidl::VectorView<uint8_t>>::FromExternal(read_vectors_.data(), read_ops));
  } else {
    completer.ReplyError(status);
  }
}

void I2cBus::CreateAndAddChildrenOnDispatcher(
    zx_device_t* parent, const ddk::I2cImplProtocolClient& i2c, const uint32_t bus_id,
    const uint64_t max_transfer_size,
    const fidl::VectorView<fuchsia_hardware_i2c_businfo::wire::I2CChannel>& channels,
    async_dispatcher_t* const dispatcher) {
  auto bus = fbl::MakeRefCounted<I2cBus>(parent, i2c, bus_id, max_transfer_size);

  for (const auto& channel : channels) {
    const uint32_t child_bus_id = channel.has_bus_id() ? channel.bus_id() : 0;
    if (bus_id == child_bus_id) {
      zx_status_t status =
          I2cChild::CreateAndAddDeviceOnDispatcher(parent, channel, bus, dispatcher);
      if (status != ZX_OK) {
        return;
      }
    }
  }
}

zx_status_t I2cBus::GrowContainersIfNeeded(
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
