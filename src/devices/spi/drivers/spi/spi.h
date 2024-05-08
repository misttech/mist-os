// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SPI_DRIVERS_SPI_SPI_H_
#define SRC_DEVICES_SPI_DRIVERS_SPI_SPI_H_

#include <fidl/fuchsia.hardware.spi.businfo/cpp/fidl.h>
#include <fidl/fuchsia.hardware.spi/cpp/fidl.h>
#include <fidl/fuchsia.hardware.spiimpl/cpp/driver/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/fdf/cpp/dispatcher.h>

#include <optional>
#include <vector>

namespace spi {

class SpiChild;

class SpiDevice : public fdf::DriverBase {
 public:
  SpiDevice(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher dispatcher)
      : fdf::DriverBase("spi", std::move(start_args), std::move(dispatcher)) {}

  void Start(fdf::StartCompleter completer) override;

 private:
  zx::result<> AddChildren(const fuchsia_hardware_spi_businfo::SpiBusMetadata& metadata,
                           fdf::WireSharedClient<fuchsia_hardware_spiimpl::SpiImpl> client);

  fdf::UnownedSynchronizedDispatcher fidl_dispatcher() const {
    if (fidl_dispatcher_) {
      return fidl_dispatcher_->borrow();
    }
    return driver_dispatcher()->borrow();
  }

  uint32_t bus_id_ = 0;
  fdf::UnownedDispatcher driver_dispatcher_;
  std::optional<fdf::SynchronizedDispatcher> fidl_dispatcher_;

  std::vector<std::unique_ptr<SpiChild>> children_;

  fidl::WireSyncClient<fuchsia_driver_framework::Node> parent_;
  fidl::WireSyncClient<fuchsia_driver_framework::NodeController> controller_;
  fidl::WireSyncClient<fuchsia_driver_framework::Node> spi_node_;
};

}  // namespace spi

#endif  // SRC_DEVICES_SPI_DRIVERS_SPI_SPI_H_
