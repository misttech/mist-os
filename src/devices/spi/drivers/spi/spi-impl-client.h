// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SPI_DRIVERS_SPI_SPI_IMPL_CLIENT_H_
#define SRC_DEVICES_SPI_DRIVERS_SPI_SPI_IMPL_CLIENT_H_

#include <fidl/fuchsia.hardware.spiimpl/cpp/driver/fidl.h>
#include <fuchsia/hardware/spiimpl/cpp/banjo.h>

namespace spi {

// Virtual base class for SpiImpl clients. Should be inherited by FIDL and Banjo implementations.
class SpiImplClient {
 public:
  virtual zx::result<uint32_t> GetChipSelectCount() = 0;
  virtual zx::result<> TransmitVector(uint32_t cs, std::vector<uint8_t> txdata) = 0;
  virtual zx::result<std::vector<uint8_t>> ReceiveVector(uint32_t cs, uint32_t size) = 0;
  virtual zx::result<std::vector<uint8_t>> ExchangeVector(uint32_t cs,
                                                          std::vector<uint8_t> txdata) = 0;
  virtual zx::result<> RegisterVmo(uint32_t chip_select, uint32_t vmo_id, fuchsia_mem::Range vmo,
                                   fuchsia_hardware_sharedmemory::SharedVmoRight rights) = 0;
  virtual zx::result<zx::vmo> UnregisterVmo(uint32_t chip_select, uint32_t vmo_id) = 0;
  virtual void ReleaseRegisteredVmos(uint32_t chip_select) = 0;
  virtual zx::result<> TransmitVmo(uint32_t chip_select,
                                   fuchsia_hardware_sharedmemory::SharedVmoBuffer buffer) = 0;
  virtual zx::result<> ReceiveVmo(uint32_t chip_select,
                                  fuchsia_hardware_sharedmemory::SharedVmoBuffer buffer) = 0;
  virtual zx::result<> ExchangeVmo(uint32_t chip_select,
                                   fuchsia_hardware_sharedmemory::SharedVmoBuffer tx_buffer,
                                   fuchsia_hardware_sharedmemory::SharedVmoBuffer rx_buffer) = 0;
  virtual zx::result<> LockBus(uint32_t chip_select) = 0;
  virtual zx::result<> UnlockBus(uint32_t chip_select) = 0;
};

// FIDL SpiImpl Client
class FidlSpiImplClient : public SpiImplClient {
 public:
  explicit FidlSpiImplClient(fdf::ClientEnd<fuchsia_hardware_spiimpl::SpiImpl> client)
      : client_(std::move(client)) {}

  zx::result<uint32_t> GetChipSelectCount() override;
  zx::result<> TransmitVector(uint32_t cs, std::vector<uint8_t> txdata) override;
  zx::result<std::vector<uint8_t>> ReceiveVector(uint32_t cs, uint32_t size) override;
  zx::result<std::vector<uint8_t>> ExchangeVector(uint32_t cs,
                                                  std::vector<uint8_t> txdata) override;
  zx::result<> RegisterVmo(uint32_t chip_select, uint32_t vmo_id, fuchsia_mem::Range vmo,
                           fuchsia_hardware_sharedmemory::SharedVmoRight rights) override;
  zx::result<zx::vmo> UnregisterVmo(uint32_t chip_select, uint32_t vmo_id) override;
  void ReleaseRegisteredVmos(uint32_t chip_select) override;
  zx::result<> TransmitVmo(uint32_t chip_select,
                           fuchsia_hardware_sharedmemory::SharedVmoBuffer buffer) override;
  zx::result<> ReceiveVmo(uint32_t chip_select,
                          fuchsia_hardware_sharedmemory::SharedVmoBuffer buffer) override;
  zx::result<> ExchangeVmo(uint32_t chip_select,
                           fuchsia_hardware_sharedmemory::SharedVmoBuffer tx_buffer,
                           fuchsia_hardware_sharedmemory::SharedVmoBuffer rx_buffer) override;
  zx::result<> LockBus(uint32_t chip_select) override;
  zx::result<> UnlockBus(uint32_t chip_select) override;

 private:
  fdf::WireSyncClient<fuchsia_hardware_spiimpl::SpiImpl> client_;
};

// Banjo SpiImpl Client
class BanjoSpiImplClient : public SpiImplClient {
 public:
  explicit BanjoSpiImplClient(ddk::SpiImplProtocolClient client) : client_(client) {}

  zx::result<uint32_t> GetChipSelectCount() override;
  zx::result<> TransmitVector(uint32_t cs, std::vector<uint8_t> txdata) override;
  zx::result<std::vector<uint8_t>> ReceiveVector(uint32_t cs, uint32_t size) override;
  zx::result<std::vector<uint8_t>> ExchangeVector(uint32_t cs,
                                                  std::vector<uint8_t> txdata) override;
  zx::result<> RegisterVmo(uint32_t chip_select, uint32_t vmo_id, fuchsia_mem::Range vmo,
                           fuchsia_hardware_sharedmemory::SharedVmoRight rights) override;
  zx::result<zx::vmo> UnregisterVmo(uint32_t chip_select, uint32_t vmo_id) override;
  void ReleaseRegisteredVmos(uint32_t chip_select) override;
  zx::result<> TransmitVmo(uint32_t chip_select,
                           fuchsia_hardware_sharedmemory::SharedVmoBuffer buffer) override;
  zx::result<> ReceiveVmo(uint32_t chip_select,
                          fuchsia_hardware_sharedmemory::SharedVmoBuffer buffer) override;
  zx::result<> ExchangeVmo(uint32_t chip_select,
                           fuchsia_hardware_sharedmemory::SharedVmoBuffer tx_buffer,
                           fuchsia_hardware_sharedmemory::SharedVmoBuffer rx_buffer) override;
  zx::result<> LockBus(uint32_t chip_select) override;
  zx::result<> UnlockBus(uint32_t chip_select) override;

 private:
  ddk::SpiImplProtocolClient client_;
};

}  // namespace spi

#endif  // SRC_DEVICES_SPI_DRIVERS_SPI_SPI_IMPL_CLIENT_H_
