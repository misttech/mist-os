// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SPI_DRIVERS_SPI_SPI_IMPL_CLIENT_H_
#define SRC_DEVICES_SPI_DRIVERS_SPI_SPI_IMPL_CLIENT_H_

#include <fidl/fuchsia.hardware.spiimpl/cpp/driver/fidl.h>
#include <fuchsia/hardware/spiimpl/cpp/banjo.h>

namespace spi {

// Banjo SpiImpl Client
class BanjoSpiImplClient {
 public:
  explicit BanjoSpiImplClient(ddk::SpiImplProtocolClient client) : client_(client) {}

  zx::result<uint32_t> GetChipSelectCount();
  zx::result<> TransmitVector(uint32_t cs, std::vector<uint8_t> txdata);
  zx::result<std::vector<uint8_t>> ReceiveVector(uint32_t cs, uint32_t size);
  zx::result<std::vector<uint8_t>> ExchangeVector(uint32_t cs, std::vector<uint8_t> txdata);
  zx::result<> RegisterVmo(uint32_t chip_select, uint32_t vmo_id, fuchsia_mem::Range vmo,
                           fuchsia_hardware_sharedmemory::SharedVmoRight rights);
  zx::result<zx::vmo> UnregisterVmo(uint32_t chip_select, uint32_t vmo_id);
  void ReleaseRegisteredVmos(uint32_t chip_select);
  zx::result<> TransmitVmo(uint32_t chip_select,
                           fuchsia_hardware_sharedmemory::SharedVmoBuffer buffer);
  zx::result<> ReceiveVmo(uint32_t chip_select,
                          fuchsia_hardware_sharedmemory::SharedVmoBuffer buffer);
  zx::result<> ExchangeVmo(uint32_t chip_select,
                           fuchsia_hardware_sharedmemory::SharedVmoBuffer tx_buffer,
                           fuchsia_hardware_sharedmemory::SharedVmoBuffer rx_buffer);
  zx::result<> LockBus(uint32_t chip_select);
  zx::result<> UnlockBus(uint32_t chip_select);

 private:
  ddk::SpiImplProtocolClient client_;
};

}  // namespace spi

#endif  // SRC_DEVICES_SPI_DRIVERS_SPI_SPI_IMPL_CLIENT_H_
