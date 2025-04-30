// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vendor/misttech/src/devices/bus/drivers/pci/allocation.h"

#include <lib/zx/result.h>
#include <trace.h>
#include <zircon/rights.h>
#include <zircon/status.h>

#include <cassert>
#include <cstring>
#include <memory>

#include <fbl/algorithm.h>
#include <vm/vm_object_physical.h>

#define LOCAL_TRACE 0

namespace pci {

zx::result<fbl::RefPtr<VmObjectDispatcher>> PciAllocation::CreateVmo() const {
  fbl::RefPtr<VmObjectPhysical> vmo;
  zx_status_t status = VmObjectPhysical::Create(base(), size(), &vmo);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  zx_rights_t rights;
  KernelHandle<VmObjectDispatcher> handle;
  status = VmObjectDispatcher::Create(vmo, size(), VmObjectDispatcher::InitialMutability::kMutable,
                                      &handle, &rights);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok(ktl::move(handle.release()));
}

zx::result<fbl::RefPtr<ResourceDispatcher>> PciAllocation::CreateResource() const {
  return zx::ok(resource_);
}

zx::result<std::unique_ptr<PciAllocation>> PciRootAllocator::Allocate(
    std::optional<zx_paddr_t> base, size_t size) {
  zx_paddr_t in_base = (base) ? *base : 0;
  zx_paddr_t out_base = {};
  uint8_t out_resource_list[1];
  uint8_t out_token_list[1];
  size_t out_resource_actual;
  size_t out_token_actual;

  zx_status_t status =
      pciroot_.GetAddressSpace(in_base, size, type(), low_, &out_base, out_resource_list, 1,
                               &out_resource_actual, out_token_list, 1, &out_token_actual);
  if (status != ZX_OK) {
    bool mmio = type() == PCI_ADDRESS_SPACE_MEMORY;
    // This error may not be fatal, the Device probe/allocation methods will know for sure.
    LTRACEF("failed to allocate %s %s [%#8lx, %#8lx) from root: %s\n", (mmio) ? "mmio" : "io",
            (mmio) ? ((low_) ? "<4GB" : ">4GB") : "", in_base, in_base + size,
            zx_status_get_string(status));
    return zx::error(status);
  }

  fbl::RefPtr<ResourceDispatcher> res =
      fbl::ImportFromRawPtr(reinterpret_cast<ResourceDispatcher*>(out_resource_list[0]));
  fbl::RefPtr<EventPairDispatcher> ep =
      fbl::ImportFromRawPtr(reinterpret_cast<EventPairDispatcher*>(out_token_list[0]));

  fbl::AllocChecker ac;
  auto allocation = std::unique_ptr<PciAllocation>(
      new (&ac) PciRootAllocation(pciroot_, type(), std::move(res), std::move(ep), out_base, size));
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  return zx::ok(std::move(allocation));
}

zx::result<std::unique_ptr<PciAllocation>> PciRegionAllocator::Allocate(
    std::optional<zx_paddr_t> base, size_t size) {
  if (!parent_alloc_) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  RegionAllocator::Region::UPtr region_uptr;
  zx_status_t status = ZX_OK;
  // Only use base if it is non-zero. RegionAllocator's interface is overloaded so we have
  // to call it differently.
  if (base) {
    ralloc_region_t request = {
        .base = (base) ? *base : 0,
        .size = size,
    };
    status = allocator_.GetRegion(request, region_uptr);
  } else {
    status = allocator_.GetRegion(size, PAGE_SIZE, region_uptr);
  }

  if (status != ZX_OK) {
    return zx::error(status);
  }

  LTRACEF("bridge: assigned [%#lx, %#lx) downstream\n", region_uptr->base,
          region_uptr->base + size);

  fbl::AllocChecker ac;
  auto allocation = std::unique_ptr<PciAllocation>(
      new (&ac) PciRegionAllocation(type(), parent_alloc_->resource(), std::move(region_uptr)));
  if (!ac.check()) {
    return zx::error(ZX_ERR_NO_MEMORY);
  }

  return zx::ok(std::move(allocation));
}

zx_status_t PciRegionAllocator::SetParentAllocation(std::unique_ptr<PciAllocation> alloc) {
  ZX_DEBUG_ASSERT(!parent_alloc_);

  parent_alloc_ = std::move(alloc);
  auto base = parent_alloc_->base();
  auto size = parent_alloc_->size();
  printf("base: %lx, size: %lx\n", base, size);
  return allocator_.AddRegion({.base = base, .size = size});
}

}  // namespace pci
