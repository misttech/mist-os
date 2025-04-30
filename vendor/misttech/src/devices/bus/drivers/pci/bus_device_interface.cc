// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <trace.h>
#include <zircon/errors.h>
#include <zircon/status.h>

#include "vendor/misttech/src/devices/bus/drivers/pci/bus.h"

#define LOCAL_TRACE 0

namespace pci {

zx_status_t Bus::LinkDevice(fbl::RefPtr<pci::Device> device) {
  fbl::AutoLock _(&devices_lock_);
  if (devices_.find(device->config()->bdf())) {
    return ZX_ERR_ALREADY_EXISTS;
  }
  devices_.insert(device);
  return ZX_OK;
}

zx_status_t Bus::UnlinkDevice(pci::Device* device) {
  fbl::AutoLock _(&devices_lock_);
  ZX_DEBUG_ASSERT(device);
  if (devices_.find(device->config()->bdf())) {
    devices_.erase(*device);
    return ZX_OK;
  }
  return ZX_ERR_NOT_FOUND;
}

zx_status_t Bus::AllocateMsi(uint32_t count, fbl::RefPtr<MsiAllocation>* msi) {
  fbl::AutoLock _(&devices_lock_);

  uint8_t allocation_list[1];
  size_t allocation_actual = 0;
  zx_status_t status = pciroot().AllocateMsi(count, false, allocation_list, 1, &allocation_actual);
  if (status != ZX_OK) {
    return status;
  }
  ZX_ASSERT(allocation_actual == 1);
  *msi = fbl::ImportFromRawPtr(reinterpret_cast<MsiAllocation*>(allocation_list[0]));
  return ZX_OK;
}

zx_status_t Bus::GetBti(const pci::Device* device, uint32_t index,
                        fbl::RefPtr<BusTransactionInitiatorDispatcher>* bti) {
  if (!device) {
    return ZX_ERR_INVALID_ARGS;
  }
  fbl::AutoLock devices_lock(&devices_lock_);
  uint8_t allocation_list[1];
  size_t allocation_actual = 0;
  zx_status_t status =
      pciroot().GetBti(device->packed_addr(), index, allocation_list, 1, &allocation_actual);
  if (status != ZX_OK) {
    return status;
  }
  ZX_ASSERT(allocation_actual == 1);
  *bti = fbl::ImportFromRawPtr(
      reinterpret_cast<BusTransactionInitiatorDispatcher*>(allocation_list[0]));
  return ZX_OK;
}

zx_status_t Bus::AddToSharedIrqList(pci::Device* device, uint32_t vector) {
  ZX_DEBUG_ASSERT(vector);
  fbl::AutoLock _(&devices_lock_);

  if (auto result = shared_irqs_.find(vector); result != shared_irqs_.end()) {
    auto& list = result->second->list;
    if (list.find_if([device](auto& iter) -> bool { return device == &iter; }) != list.end()) {
      return ZX_ERR_ALREADY_EXISTS;
    }
    list.push_back(device);
    LTRACEF("[%s] inserted into list for vector %#x\n", device->config()->addr(), vector);
    return ZX_OK;
  }
  return ZX_ERR_BAD_STATE;
}

zx_status_t Bus::RemoveFromSharedIrqList(pci::Device* device, uint32_t vector) {
  ZX_DEBUG_ASSERT(vector);
  fbl::AutoLock _(&devices_lock_);

  if (auto result = shared_irqs_.find(vector); result != shared_irqs_.end()) {
    auto& list = result->second->list;
    if (list.erase(*device) != nullptr) {
      LTRACEF("[%s] removed from vector %#x list\n", device->config()->addr(), vector);
      return ZX_OK;
    }
    return ZX_ERR_NOT_FOUND;
  }
  return ZX_ERR_BAD_STATE;
}

}  // namespace pci
