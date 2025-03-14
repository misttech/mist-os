// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <utility>

#include <acpica/acpi.h>
#include <fbl/auto_lock.h>
#include <fbl/intrusive_hash_table.h>
#include <fbl/intrusive_single_list.h>
#include <fbl/mutex.h>
#include <vm/vm_address_region.h>
#include <vm/vm_aspace.h>
#include <vm/vm_object.h>
#include <vm/vm_object_physical.h>

#include "zircon/system/ulib/acpica/oszircon.h"

namespace {
class AcpiOsMappingNode : public fbl::SinglyLinkedListable<std::unique_ptr<AcpiOsMappingNode>> {
 public:
  using HashTable = fbl::HashTable<uintptr_t, std::unique_ptr<AcpiOsMappingNode>>;

  // @param vaddr Virtual address returned to ACPI, used as key to the hashtable.
  // @param mapping Handle to the mapped VMO
  AcpiOsMappingNode(uintptr_t vaddr, fbl::RefPtr<VmMapping> mapping);
  ~AcpiOsMappingNode();

  // Trait implementation for fbl::HashTable
  uintptr_t GetKey() const { return vaddr_; }
  static size_t GetHash(uintptr_t key) { return key; }

 private:
  uintptr_t vaddr_;
  fbl::RefPtr<VmMapping> mapping_;
};

AcpiOsMappingNode::AcpiOsMappingNode(uintptr_t vaddr, fbl::RefPtr<VmMapping> mapping)
    : vaddr_(vaddr), mapping_(std::move(mapping)) {}

AcpiOsMappingNode::~AcpiOsMappingNode() {
  if (mapping_) {
    mapping_->Destroy();
  }
}

fbl::Mutex os_mapping_lock;
AcpiOsMappingNode::HashTable os_mapping_tbl;
}  // namespace

static zx_status_t mmap_physical(zx_paddr_t phys, size_t size, uint32_t cache_policy,
                                 fbl::RefPtr<VmMapping>* out_mapping, zx_vaddr_t* out_vaddr) {
  zx_status_t status = VmObject::RoundSize(size, &size);
  if (status != ZX_OK) {
    return status;
  }

  // create a vm object
  fbl::RefPtr<VmObjectPhysical> vmo;
  status = VmObjectPhysical::Create(phys, size, &vmo);
  if (status != ZX_OK) {
    return status;
  }

  status = vmo->SetMappingCachePolicy(ZX_CACHE_POLICY_CACHED);
  if (status != ZX_OK) {
    return status;
  }

  zx::result<VmAddressRegion::MapResult> mapping_result =
      VmAspace::kernel_aspace()->RootVmar()->CreateVmMapping(
          0, size, 0, 0 /* vmar_flags */, ktl::move(vmo), 0,
          ARCH_MMU_FLAG_CACHED | ARCH_MMU_FLAG_PERM_READ | ARCH_MMU_FLAG_PERM_WRITE, "acpica");
  if (mapping_result.is_error()) {
    return mapping_result.error_value();
  }

  // Prepopulate the mapping's page tables so there are no page faults taken.
  status = mapping_result->mapping->MapRange(0, size, true);
  if (status != ZX_OK) {
    return status;
  }

  *out_mapping = ktl::move(mapping_result->mapping);
  *out_vaddr = mapping_result->base;
  return ZX_OK;
}

/**
 * @brief Map physical memory into the caller's address space.
 *
 * @param PhysicalAddress A full physical address of the memory to be mapped
 *        into the caller's address space
 * @param Length The amount of memory to mapped starting at the given physical
 *        address
 *
 * @return Logical pointer to the mapped memory. A NULL pointer indicated failures.
 */
void* AcpiOsMapMemory(ACPI_PHYSICAL_ADDRESS PhysicalAddress, ACPI_SIZE Length) {
  // LTRACEF("PhysicalAddress: %llx, Length: %llu\n", PhysicalAddress, Length);
  fbl::AutoLock lock(&os_mapping_lock);

  // Caution: PhysicalAddress might not be page-aligned, Length might not
  // be a page multiple.

  const size_t kPageSize = PAGE_SIZE;
  ACPI_PHYSICAL_ADDRESS aligned_address = PhysicalAddress & ~(kPageSize - 1);
  ACPI_PHYSICAL_ADDRESS end = (PhysicalAddress + Length + kPageSize - 1) & ~(kPageSize - 1);

  uintptr_t vaddr;
  fbl::RefPtr<VmMapping> mapping;
  zx_status_t status = mmap_physical(aligned_address, end - aligned_address, ZX_CACHE_POLICY_CACHED,
                                     &mapping, &vaddr);
  if (status != ZX_OK) {
    return NULL;
  }

  void* out_addr = (void*)(vaddr + (PhysicalAddress - aligned_address));
  fbl::AllocChecker ac;
  ktl::unique_ptr<AcpiOsMappingNode> mn =
      ktl::make_unique<AcpiOsMappingNode>(&ac, reinterpret_cast<uintptr_t>(out_addr), mapping);
  if (!ac.check()) {
    return NULL;
  }
  os_mapping_tbl.insert(std::move(mn));

  return out_addr;
}

/**
 * @brief Remove a physical to logical memory mapping.
 *
 * @param LogicalAddress The logical address that was returned from a previous
 *        call to AcpiOsMapMemory.
 * @param Length The amount of memory that was mapped. This value must be
 *        identical to the value used in the call to AcpiOsMapMemory.
 */
void AcpiOsUnmapMemory(void* LogicalAddress, ACPI_SIZE Length) {
  // LTRACEF("LogicalAddress: %p, Length: %llu\n", LogicalAddress, Length);
  fbl::AutoLock lock(&os_mapping_lock);
  ktl::unique_ptr<AcpiOsMappingNode> mn = os_mapping_tbl.erase((uintptr_t)LogicalAddress);
  if (mn == NULL) {
    printf("AcpiOsUnmapMemory nonexisting mapping %p\n", LogicalAddress);
  }
}
