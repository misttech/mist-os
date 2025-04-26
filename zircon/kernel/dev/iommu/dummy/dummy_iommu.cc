// Copyright 2017 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#include <align.h>
#include <zircon/errors.h>
#include <zircon/types.h>

#include <new>

#include <dev/iommu/dummy.h>
#include <fbl/ref_ptr.h>
#include <ktl/algorithm.h>
#include <ktl/utility.h>
#include <vm/vm.h>

#include <ktl/enforce.h>

#define INVALID_PADDR UINT64_MAX

DummyIommu::DummyIommu() {}

zx_status_t DummyIommu::Create(ktl::unique_ptr<const uint8_t[]> desc, size_t desc_len,
                               fbl::RefPtr<Iommu>* out) {
  if (desc_len != sizeof(zx_iommu_desc_dummy_t)) {
    return ZX_ERR_INVALID_ARGS;
  }

  fbl::AllocChecker ac;
  auto instance = fbl::AdoptRef<DummyIommu>(new (&ac) DummyIommu());
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  *out = ktl::move(instance);
  return ZX_OK;
}

DummyIommu::~DummyIommu() {}

bool DummyIommu::IsValidBusTxnId(uint64_t bus_txn_id) const { return true; }

zx::result<uint64_t> DummyIommu::Map(uint64_t bus_txn_id, const fbl::RefPtr<VmObject>& vmo,
                                     uint64_t vmo_offset, size_t size, uint32_t perms) {
  if (!IS_PAGE_ALIGNED(vmo_offset) || size == 0) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (perms & ~(IOMMU_FLAG_PERM_READ | IOMMU_FLAG_PERM_WRITE | IOMMU_FLAG_PERM_EXECUTE)) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (perms == 0) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  // Return the vmo offset as our token for use in future QueryAddress calls.
  return zx::ok(vmo_offset);
}

zx::result<uint64_t> DummyIommu::MapContiguous(uint64_t bus_txn_id,
                                               const fbl::RefPtr<VmObject>& vmo,
                                               uint64_t vmo_offset, size_t size, uint32_t perms) {
  if (!IS_PAGE_ALIGNED(vmo_offset) || size == 0) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (perms & ~(IOMMU_FLAG_PERM_READ | IOMMU_FLAG_PERM_WRITE | IOMMU_FLAG_PERM_EXECUTE)) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }
  if (perms == 0) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  // Ensure the VMO is contiguous for the range being mapped.
  paddr_t paddr = INVALID_PADDR;
  zx_status_t status = vmo->LookupContiguous(vmo_offset, size, &paddr);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  DEBUG_ASSERT(paddr != INVALID_PADDR);
  // Return the vmo offset as our token for use in future QueryAddress calls.
  return zx::ok(vmo_offset);
}

zx_status_t DummyIommu::QueryAddress(uint64_t bus_txn_id, const fbl::RefPtr<VmObject>& vmo,
                                     uint64_t map_token, uint64_t map_offset, size_t size,
                                     dev_vaddr_t* vaddr, size_t* mapped_len) {
  DEBUG_ASSERT(vaddr);
  DEBUG_ASSERT(mapped_len);
  if (!IS_PAGE_ALIGNED(map_token) || !IS_PAGE_ALIGNED(map_offset) || size == 0) {
    return ZX_ERR_INVALID_ARGS;
  }
  const uint64_t offset = map_token + map_offset;
  paddr_t paddr = INVALID_PADDR;
  size = ROUNDUP(size, PAGE_SIZE);
  zx_status_t status = vmo->LookupContiguous(offset, size, &paddr);
  // If the range is fundamentally incorrect or out of range then we immediately error. Otherwise
  // even if we have some other error case we will fall back to attempting single pages at a time.
  if (status == ZX_ERR_INVALID_ARGS || status == ZX_ERR_OUT_OF_RANGE) {
    return status;
  }
  if (status == ZX_OK) {
    DEBUG_ASSERT(paddr != INVALID_PADDR);
    *vaddr = paddr;
    *mapped_len = size;
    return ZX_OK;
  }

  status = vmo->LookupContiguous(offset, PAGE_SIZE, &paddr);
  if (status != ZX_OK) {
    return status;
  }
  DEBUG_ASSERT(paddr != INVALID_PADDR);
  *vaddr = paddr;
  *mapped_len = PAGE_SIZE;
  return ZX_OK;
}

zx_status_t DummyIommu::Unmap(uint64_t bus_txn_id, uint64_t map_token, size_t size) {
  if (!IS_PAGE_ALIGNED(map_token) || !IS_PAGE_ALIGNED(size)) {
    return ZX_ERR_INVALID_ARGS;
  }
  return ZX_OK;
}

zx_status_t DummyIommu::ClearMappingsForBusTxnId(uint64_t bus_txn_id) { return ZX_OK; }

uint64_t DummyIommu::minimum_contiguity(uint64_t bus_txn_id) { return PAGE_SIZE; }

uint64_t DummyIommu::aspace_size(uint64_t bus_txn_id) { return UINT64_MAX; }
