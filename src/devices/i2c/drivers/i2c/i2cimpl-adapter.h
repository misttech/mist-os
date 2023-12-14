// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_I2C_I2CIMPL_ADAPTER_H_
#define SRC_DEVICES_I2C_DRIVERS_I2C_I2CIMPL_ADAPTER_H_

#include <fidl/fuchsia.hardware.i2cimpl/cpp/driver/wire.h>
#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/stdcompat/span.h>

#include <cstring>
#include <memory>
#include <vector>

#include <ddktl/device.h>

namespace i2c {

// A TransportAdapter allows for a common interface to dispatch to various protocol transports.
class TransportAdapter {
 public:
  virtual ~TransportAdapter() = default;

  // Device protocol methods.
  virtual zx_status_t GetMaxTransferSize(uint64_t* size) = 0;
  virtual zx_status_t SetBitrate(uint32_t bitrate) = 0;
  virtual zx_status_t Transact(std::vector<i2c_impl_op_t>& ops, size_t count) = 0;

  virtual bool is_valid() = 0;
};

// A TransportAdapter for the banjo transport. This is simply a transparent wrapper around a
// ddk::I2cImplProtocolClient. Inputs are given as banjo primitives and simply passed to the
// underlying protocol client.
class BanjoTransportAdapter : public TransportAdapter {
 public:
  explicit BanjoTransportAdapter(zx_device_t* parent) : i2cimpl_{parent} {}

  zx_status_t GetMaxTransferSize(uint64_t* size) override {
    return i2cimpl_.GetMaxTransferSize(size);
  }

  zx_status_t SetBitrate(uint32_t bitrate) override { return i2cimpl_.SetBitrate(bitrate); }

  zx_status_t Transact(std::vector<i2c_impl_op_t>& ops, size_t count) override {
    return i2cimpl_.Transact(ops.data(), count);
  }

  bool is_valid() override { return i2cimpl_.is_valid(); }

 private:
  const ddk::I2cImplProtocolClient i2cimpl_;
};

// A TransportAdapter for the FIDL transport. The banjo-style interface is transformed into the
// requisite FIDL operations, and then executed on a fidl::SyncClient<fi2cimpl::Device>.
class FidlTransportAdapter : public TransportAdapter {
 public:
  explicit FidlTransportAdapter(fdf::ClientEnd<fuchsia_hardware_i2cimpl::Device>&& client_end)
      : i2cimpl_{std::move(client_end)} {}

  zx_status_t GetMaxTransferSize(uint64_t* size) override {
    fdf::Arena arena('I2C0');
    auto result = i2cimpl_.buffer(arena)->GetMaxTransferSize();
    if (!result.ok()) {
      return result.status();  // framework error.
    } else if (result->is_error()) {
      return result->error_value();
    }
    *size = result->value()->size;
    return ZX_OK;
  }

  zx_status_t SetBitrate(uint32_t bitrate) override {
    fdf::Arena arena('I2C0');
    auto result = i2cimpl_.buffer(arena)->SetBitrate(bitrate);
    if (!result.ok()) {
      return result.status();  // framework error.
    }
    return result->is_error() ? result->error_value() : ZX_OK;
  }

  zx_status_t Transact(std::vector<i2c_impl_op_t>& ops, size_t count) override {
    fdf::Arena arena('I2C0');
    fidl::VectorView<fuchsia_hardware_i2cimpl::wire::I2cImplOp> fidl_ops{arena, count};

    // Here, we have to create a FIDL I2cImplOp from the given banjo i2c_impl_op_t. While they are
    // field-equivalent, they are also different types. This marshals the data from the banjo type
    // into the FIDL type.
    for (size_t i = 0; i < count; i++) {
      auto& banjo_op = ops[i];

      cpp20::span<uint8_t> data{&banjo_op.data_buffer[0], banjo_op.data_size};

      fuchsia_hardware_i2cimpl::wire::I2cImplOpType op_type =
          banjo_op.is_read ? fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithReadSize(
                                 static_cast<uint32_t>(data.size()))
                           : fuchsia_hardware_i2cimpl::wire::I2cImplOpType::WithWriteData(
                                 fidl::ObjectView<fidl::VectorView<uint8_t>>{
                                     arena, fidl::VectorView<uint8_t>{arena, data}});
      fuchsia_hardware_i2cimpl::wire::I2cImplOp fidl_op{
          .address = banjo_op.address,
          .type = op_type,
          .stop = banjo_op.stop,
      };

      fidl_ops[i] = std::move(fidl_op);
    }

    auto result = i2cimpl_.buffer(arena)->Transact(fidl_ops);
    if (!result.ok()) {
      return result.status();  //  framework error.
    } else if (result->is_error()) {
      return result->error_value();
    }

    // In the case of read data, we need to copy back to the user's buffer.
    int data_idx = 0;
    for (size_t i = 0; i < count; i++) {
      auto& banjo_op = ops[i];
      if (!banjo_op.is_read) {
        continue;  // Write transaction, no resulting read data to return.
      }

      cpp20::span<uint8_t> read_data = result.value()->read[data_idx++].data.get();

      std::memcpy(banjo_op.data_buffer, read_data.data(), read_data.size());
    }
    return ZX_OK;
  }

  bool is_valid() override { return i2cimpl_.is_valid(); }

 private:
  fdf::WireSyncClient<fuchsia_hardware_i2cimpl::Device> i2cimpl_;
};

}  // namespace i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_I2C_I2CIMPL_ADAPTER_H_
