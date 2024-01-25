// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "spi-impl-client.h"

#include <lib/ddk/debug.h>
#include <zircon/errors.h>

namespace spi {

zx::result<uint32_t> BanjoSpiImplClient::GetChipSelectCount() {
  return zx::ok(client_.GetChipSelectCount());
}

zx::result<> BanjoSpiImplClient::TransmitVector(uint32_t cs, std::vector<uint8_t> txdata) {
  size_t actual;
  auto status = client_.Exchange(cs, txdata.data(), txdata.size(), nullptr, 0, &actual);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiImpl::Exchange failed %d", status);
    return zx::error(status);
  }

  return zx::ok();
}

zx::result<std::vector<uint8_t>> BanjoSpiImplClient::ReceiveVector(uint32_t cs, uint32_t size) {
  std::vector<uint8_t> rxdata(size);
  size_t actual;
  auto status = client_.Exchange(cs, nullptr, 0, rxdata.data(), rxdata.size(), &actual);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiImpl::Exchange failed %d", status);
    return zx::error(status);
  }
  if (actual != rxdata.size()) {
    zxlogf(ERROR, "Expected to receive %zu bytes but received %zu bytes", rxdata.size(), actual);
    return zx::error(ZX_ERR_INTERNAL);
  }

  return zx::ok(std::move(rxdata));
}

zx::result<std::vector<uint8_t>> BanjoSpiImplClient::ExchangeVector(uint32_t cs,
                                                                    std::vector<uint8_t> txdata) {
  std::vector<uint8_t> rxdata(txdata.size());
  size_t actual;
  auto status =
      client_.Exchange(cs, txdata.data(), txdata.size(), rxdata.data(), rxdata.size(), &actual);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiImpl::Exchange failed %d", status);
    return zx::error(status);
  }
  if (actual != rxdata.size()) {
    zxlogf(ERROR, "Expected to receive %zu bytes but received %zu bytes", rxdata.size(), actual);
    return zx::error(ZX_ERR_INTERNAL);
  }

  return zx::ok(std::move(rxdata));
}

zx::result<> BanjoSpiImplClient::RegisterVmo(uint32_t chip_select, uint32_t vmo_id,
                                             fuchsia_mem::Range vmo,
                                             fuchsia_hardware_sharedmemory::SharedVmoRight rights) {
  auto status = client_.RegisterVmo(chip_select, vmo_id, std::move(vmo.vmo()), vmo.offset(),
                                    vmo.size(), static_cast<uint32_t>(rights));
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiImpl::RegisterVmo failed %d", status);
    return zx::error(status);
  }

  return zx::ok();
}

zx::result<zx::vmo> BanjoSpiImplClient::UnregisterVmo(uint32_t chip_select, uint32_t vmo_id) {
  zx::vmo vmo;
  auto status = client_.UnregisterVmo(chip_select, vmo_id, &vmo);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiImpl::UnregisterVmo failed %d", status);
    return zx::error(status);
  }

  return zx::ok(std::move(vmo));
}

void BanjoSpiImplClient::ReleaseRegisteredVmos(uint32_t chip_select) {
  client_.ReleaseRegisteredVmos(chip_select);
}

zx::result<> BanjoSpiImplClient::TransmitVmo(
    uint32_t chip_select, fuchsia_hardware_sharedmemory::SharedVmoBuffer buffer) {
  auto status = client_.TransmitVmo(chip_select, buffer.vmo_id(), buffer.offset(), buffer.size());
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiImpl::TransmitVmo failed %d", status);
    return zx::error(status);
  }

  return zx::ok();
}

zx::result<> BanjoSpiImplClient::ReceiveVmo(uint32_t chip_select,
                                            fuchsia_hardware_sharedmemory::SharedVmoBuffer buffer) {
  auto status = client_.ReceiveVmo(chip_select, buffer.vmo_id(), buffer.offset(), buffer.size());
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiImpl::ReceiveVmo failed %d", status);
    return zx::error(status);
  }

  return zx::ok();
}

zx::result<> BanjoSpiImplClient::ExchangeVmo(
    uint32_t chip_select, fuchsia_hardware_sharedmemory::SharedVmoBuffer tx_buffer,
    fuchsia_hardware_sharedmemory::SharedVmoBuffer rx_buffer) {
  if (tx_buffer.size() != rx_buffer.size()) {
    zxlogf(ERROR, "Buffers must match in size %zu (tx) != %zu (rx)", tx_buffer.size(),
           rx_buffer.size());
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  auto status = client_.ExchangeVmo(chip_select, tx_buffer.vmo_id(), tx_buffer.offset(),
                                    rx_buffer.vmo_id(), rx_buffer.offset(), tx_buffer.size());
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiImpl::ReceiveVmo failed %d", status);
    return zx::error(status);
  }

  return zx::ok();
}

zx::result<> BanjoSpiImplClient::LockBus(uint32_t chip_select) {
  auto status = client_.LockBus(chip_select);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiImpl::LockBus failed %d", status);
    return zx::error(status);
  }

  return zx::ok();
}

zx::result<> BanjoSpiImplClient::UnlockBus(uint32_t chip_select) {
  auto status = client_.UnlockBus(chip_select);
  if (status != ZX_OK) {
    zxlogf(ERROR, "SpiImpl::UnlockBus failed %d", status);
    return zx::error(status);
  }

  return zx::ok();
}

}  // namespace spi
