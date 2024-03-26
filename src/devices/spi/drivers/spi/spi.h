// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SPI_DRIVERS_SPI_SPI_H_
#define SRC_DEVICES_SPI_DRIVERS_SPI_SPI_H_

#include <fidl/fuchsia.hardware.spi.businfo/cpp/wire.h>
#include <fidl/fuchsia.hardware.spi/cpp/fidl.h>
#include <fidl/fuchsia.hardware.spiimpl/cpp/driver/wire.h>
#include <lib/fdf/cpp/dispatcher.h>

#include <optional>

#include <ddktl/device.h>

namespace spi {

class SpiDevice;
using SpiDeviceType = ddk::Device<SpiDevice, ddk::Unbindable>;

class SpiDevice : public SpiDeviceType {
 public:
  SpiDevice(zx_device_t* parent, uint32_t bus_id)
      : SpiDeviceType(parent),
        bus_id_(bus_id),
        driver_dispatcher_(fdf::Dispatcher::GetCurrent()->borrow()) {}

  static zx_status_t Create(void* ctx, zx_device_t* parent);
  zx_status_t Init();

  void DdkRelease();
  void DdkUnbind(ddk::UnbindTxn txn);

 private:
  using FidlClientType = fdf::WireSharedClient<fuchsia_hardware_spiimpl::SpiImpl>;

  void AddChildren(const fuchsia_hardware_spi_businfo::wire::SpiBusMetadata& metadata,
                   fdf::WireSharedClient<fuchsia_hardware_spiimpl::SpiImpl> client);

  void FidlClientTeardownHandler();
  void DispatcherShutdownHandler(fdf_dispatcher_t* dispatcher);

  fdf::UnownedDispatcher fidl_dispatcher() const {
    if (fidl_dispatcher_) {
      return fdf::UnownedDispatcher{fidl_dispatcher_->get()};
    }
    return driver_dispatcher_->borrow();
  }

  const uint32_t bus_id_;
  const fdf::UnownedDispatcher driver_dispatcher_;
  std::optional<fdf::SynchronizedDispatcher> fidl_dispatcher_;
  fdf::WireSharedClient<fuchsia_hardware_spiimpl::SpiImpl> spi_impl_;
  std::optional<ddk::UnbindTxn> unbind_txn_;
  bool fidl_client_teardown_complete_ = false;
  bool dispatcher_shutdown_complete_ = false;
};

}  // namespace spi

#endif  // SRC_DEVICES_SPI_DRIVERS_SPI_SPI_H_
