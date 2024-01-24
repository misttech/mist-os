// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SPI_DRIVERS_SPI_SPI_H_
#define SRC_DEVICES_SPI_DRIVERS_SPI_SPI_H_

#include <fidl/fuchsia.hardware.spi.businfo/cpp/wire.h>
#include <fidl/fuchsia.hardware.spi/cpp/fidl.h>
#include <fidl/fuchsia.hardware.spiimpl/cpp/wire.h>
#include <fuchsia/hardware/spiimpl/cpp/banjo.h>

#include <ddktl/device.h>

#include "src/devices/spi/drivers/spi/spi-impl-client.h"

namespace spi {

class SpiDevice;
using SpiDeviceType = ddk::Device<SpiDevice>;

class SpiDevice : public SpiDeviceType {
 public:
  SpiDevice(zx_device_t* parent, uint32_t bus_id) : SpiDeviceType(parent), bus_id_(bus_id) {}

  static zx_status_t Create(void* ctx, zx_device_t* parent, async_dispatcher_t* dispatcher);
  zx_status_t Init();

  void DdkRelease();

 private:
  template <typename T>
  void AddChildren(async_dispatcher_t* dispatcher,
                   const fuchsia_hardware_spi_businfo::wire::SpiBusMetadata& metadata);

  SpiImplClient* GetSpiImpl() {
    return std::visit([](auto&& impl) { return static_cast<SpiImplClient*>(&impl); },
                      spi_impl_.value());
  }

  const uint32_t bus_id_;
  std::optional<std::variant<FidlSpiImplClient, BanjoSpiImplClient>> spi_impl_;
};

}  // namespace spi

#endif  // SRC_DEVICES_SPI_DRIVERS_SPI_SPI_H_
