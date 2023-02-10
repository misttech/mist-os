// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_I2C_I2C_BUS_H_
#define SRC_DEVICES_I2C_DRIVERS_I2C_I2C_BUS_H_

#include <fidl/fuchsia.hardware.i2c.businfo/cpp/wire.h>
#include <fidl/fuchsia.hardware.i2c/cpp/wire.h>
#include <fuchsia/hardware/i2cimpl/cpp/banjo.h>
#include <lib/async/dispatcher.h>

#include <vector>

#include <fbl/ref_counted.h>

namespace i2c {

class I2cBus : public fbl::RefCounted<I2cBus> {
 public:
  using TransferRequestView = fidl::WireServer<fuchsia_hardware_i2c::Device>::TransferRequestView;
  using TransferCompleter = fidl::WireServer<fuchsia_hardware_i2c::Device>::TransferCompleter;

  I2cBus(zx_device_t* parent, ddk::I2cImplProtocolClient i2c, uint32_t bus_id,
         uint64_t max_transfer_size)
      : i2c_(i2c), bus_id_(bus_id), max_transfer_(max_transfer_size) {
    impl_ops_.resize(kInitialOpCount);
    read_vectors_.resize(kInitialOpCount);
    read_buffer_.resize(kInitialReadBufferSize);
  }

  static zx_status_t CreateAndAddChildren(
      zx_device_t* parent, const ddk::I2cImplProtocolClient& i2c, uint32_t bus_id,
      const fidl::VectorView<fuchsia_hardware_i2c_businfo::wire::I2CChannel>& channels,
      async_dispatcher_t* dispatcher);

  void Transact(uint16_t address, TransferRequestView request, TransferCompleter::Sync& completer);

 private:
  static constexpr size_t kInitialOpCount = 16;
  static constexpr size_t kInitialReadBufferSize = 512;

  static void CreateAndAddChildrenOnDispatcher(
      zx_device_t* parent, const ddk::I2cImplProtocolClient& i2c, uint32_t bus_id,
      uint64_t max_transfer_size,
      const fidl::VectorView<fuchsia_hardware_i2c_businfo::wire::I2CChannel>& channels,
      async_dispatcher_t* dispatcher);

  zx_status_t GrowContainersIfNeeded(
      const fidl::VectorView<fuchsia_hardware_i2c::wire::Transaction>& transactions);

  const ddk::I2cImplProtocolClient i2c_;
  const uint32_t bus_id_;
  const uint64_t max_transfer_;

  // Ops and read data/vectors to be used in Transact(). Set to the initial capacities specified
  // above; more space is dyamically allocated if needed.
  std::vector<i2c_impl_op_t> impl_ops_;
  std::vector<fidl::VectorView<uint8_t>> read_vectors_;
  std::vector<uint8_t> read_buffer_;
};

}  // namespace i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_I2C_I2C_BUS_H_
