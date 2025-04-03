// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_I2C_DRIVERS_I2C_I2C_H_
#define SRC_DEVICES_I2C_DRIVERS_I2C_I2C_H_

#include <fidl/fuchsia.hardware.i2c.businfo/cpp/fidl.h>
#include <fidl/fuchsia.hardware.i2cimpl/cpp/driver/fidl.h>
#include <lib/driver/component/cpp/driver_base.h>

#include "src/devices/i2c/drivers/i2c/i2c-child-server.h"

namespace i2c {

class I2cDriver : public fdf::DriverBase {
  using TransferRequestView = fidl::WireServer<fuchsia_hardware_i2c::Device>::TransferRequestView;
  using TransferCompleter = fidl::WireServer<fuchsia_hardware_i2c::Device>::TransferCompleter;

  static constexpr std::string_view kDriverName = "i2c";

 public:
  I2cDriver(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase(kDriverName, std::move(start_args), std::move(driver_dispatcher)) {
    impl_ops_.resize(kInitialOpCount);
    read_vectors_.resize(kInitialOpCount);
    read_buffer_.resize(kInitialReadBufferSize);
  }

  zx::result<> Start() override;

  void Transact(uint16_t address, TransferRequestView request, TransferCompleter::Sync& completer);

 private:
  static constexpr size_t kInitialOpCount = 16;
  static constexpr size_t kInitialReadBufferSize = 512;

  zx::result<> AddI2cChildren(const fuchsia_hardware_i2c_businfo::I2CBusMetadata& metadata);

  zx_status_t GrowContainersIfNeeded(
      const fidl::VectorView<fuchsia_hardware_i2c::wire::Transaction>& transactions);

  uint64_t max_transfer_;

  // Ops and read data/vectors to be used in Transact(). Set to the initial capacities specified
  // above; more space is dyamically allocated if needed.
  std::vector<fuchsia_hardware_i2cimpl::wire::I2cImplOp> impl_ops_;
  std::vector<fidl::VectorView<uint8_t>> read_vectors_;
  std::vector<uint8_t> read_buffer_;

  fdf::WireSyncClient<fuchsia_hardware_i2cimpl::Device> i2c_;

  std::vector<std::unique_ptr<I2cChildServer>> child_servers_;

  fidl::ClientEnd<fuchsia_driver_framework::Node> i2c_node_;
};

}  // namespace i2c

#endif  // SRC_DEVICES_I2C_DRIVERS_I2C_I2C_H_
