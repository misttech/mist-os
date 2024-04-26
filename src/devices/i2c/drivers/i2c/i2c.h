// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_I2C_I2C_H_
#define SRC_DEVICES_I2C_DRIVERS_I2C_I2C_H_

#include <fidl/fuchsia.hardware.i2c.businfo/cpp/wire.h>
#include <fidl/fuchsia.hardware.i2cimpl/cpp/driver/wire.h>

#include <memory>
#include <vector>

#include <ddktl/device.h>

namespace i2c {

class I2cDevice;
using I2cDeviceType = ddk::Device<I2cDevice>;

class I2cDevice : public I2cDeviceType {
 public:
  using TransferRequestView = fidl::WireServer<fuchsia_hardware_i2c::Device>::TransferRequestView;
  using TransferCompleter = fidl::WireServer<fuchsia_hardware_i2c::Device>::TransferCompleter;

  I2cDevice(zx_device_t* parent, uint64_t max_transfer_size,
            fdf::ClientEnd<fuchsia_hardware_i2cimpl::Device> i2c)
      : I2cDeviceType(parent), i2c_(std::move(i2c)), max_transfer_(max_transfer_size) {
    impl_ops_.resize(kInitialOpCount);
    read_vectors_.resize(kInitialOpCount);
    read_buffer_.resize(kInitialReadBufferSize);
  }

  static zx_status_t Create(void* ctx, zx_device_t* parent);

  void DdkRelease() { delete this; }

  void Transact(uint16_t address, TransferRequestView request, TransferCompleter::Sync& completer);

 private:
  static constexpr size_t kInitialOpCount = 16;
  static constexpr size_t kInitialReadBufferSize = 512;

  zx_status_t Init(const fuchsia_hardware_i2c_businfo::wire::I2CBusMetadata& metadata);
  void SetSchedulerRole();

  zx_status_t GrowContainersIfNeeded(
      const fidl::VectorView<fuchsia_hardware_i2c::wire::Transaction>& transactions);

  fdf::WireSyncClient<fuchsia_hardware_i2cimpl::Device> i2c_;
  const uint64_t max_transfer_;

  // Ops and read data/vectors to be used in Transact(). Set to the initial capacities specified
  // above; more space is dyamically allocated if needed.
  std::vector<fuchsia_hardware_i2cimpl::wire::I2cImplOp> impl_ops_;
  std::vector<fidl::VectorView<uint8_t>> read_vectors_;
  std::vector<uint8_t> read_buffer_;
};

}  // namespace i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_I2C_I2C_H_
