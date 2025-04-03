// Copyright 2025 Mist Tecnologia Ltda. All rights reserved.
// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fzl/vmo-mapper.h>
#include <zircon/assert.h>
#include <zircon/features.h>

#include <utility>

#include <object/vm_address_region_dispatcher.h>
#include <object/vm_object_dispatcher.h>
#include <vm/vm_object.h>
#include <vm/vm_object_paged.h>

namespace fzl {

namespace {

template <uint32_t FromFlag, uint32_t ToFlag>
uint32_t ExtractFlag(uint32_t* flags) {
  const uint32_t flag_set = *flags & FromFlag;
  // Unconditionally clear |flags| so that the compiler can more easily see that multiple
  // ExtractFlag invocations can just use a single combined clear, greatly reducing code-gen.
  *flags &= ~FromFlag;
  if (flag_set) {
    return ToFlag;
  }
  return 0;
}

// Split out the syscall flags into vmar flags and mmu flags.  Note that this
// does not validate that the requested protections in *flags* are valid.  For
// that use is_valid_mapping_protection()
zx_status_t split_syscall_flags(uint32_t flags, uint32_t* vmar_flags, uint* arch_mmu_flags,
                                uint8_t* align_pow2) {
  // Figure out arch_mmu_flags
  uint mmu_flags = 0;
  mmu_flags |= ExtractFlag<ZX_VM_PERM_READ, ARCH_MMU_FLAG_PERM_READ>(&flags);
  mmu_flags |= ExtractFlag<ZX_VM_PERM_WRITE, ARCH_MMU_FLAG_PERM_WRITE>(&flags);
  mmu_flags |= ExtractFlag<ZX_VM_PERM_EXECUTE, ARCH_MMU_FLAG_PERM_EXECUTE>(&flags);

  // This flag is no longer needed and should have already been acted upon.
  ExtractFlag<ZX_VM_PERM_READ_IF_XOM_UNSUPPORTED, 0>(&flags);

  // Figure out vmar flags
  uint32_t vmar = 0;
  vmar |= ExtractFlag<ZX_VM_COMPACT, VMAR_FLAG_COMPACT>(&flags);
  vmar |= ExtractFlag<ZX_VM_SPECIFIC, VMAR_FLAG_SPECIFIC>(&flags);
  vmar |= ExtractFlag<ZX_VM_SPECIFIC_OVERWRITE, VMAR_FLAG_SPECIFIC_OVERWRITE>(&flags);
  vmar |= ExtractFlag<ZX_VM_CAN_MAP_SPECIFIC, VMAR_FLAG_CAN_MAP_SPECIFIC>(&flags);
  vmar |= ExtractFlag<ZX_VM_CAN_MAP_READ, VMAR_FLAG_CAN_MAP_READ>(&flags);
  vmar |= ExtractFlag<ZX_VM_CAN_MAP_WRITE, VMAR_FLAG_CAN_MAP_WRITE>(&flags);
  vmar |= ExtractFlag<ZX_VM_CAN_MAP_EXECUTE, VMAR_FLAG_CAN_MAP_EXECUTE>(&flags);
  vmar |= ExtractFlag<ZX_VM_REQUIRE_NON_RESIZABLE, VMAR_FLAG_REQUIRE_NON_RESIZABLE>(&flags);
  vmar |= ExtractFlag<ZX_VM_ALLOW_FAULTS, VMAR_FLAG_ALLOW_FAULTS>(&flags);
  vmar |= ExtractFlag<ZX_VM_OFFSET_IS_UPPER_LIMIT, VMAR_FLAG_OFFSET_IS_UPPER_LIMIT>(&flags);

  if (flags & ((1u << ZX_VM_ALIGN_BASE) - 1u)) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Figure out alignment.
  uint8_t alignment = static_cast<uint8_t>(flags >> ZX_VM_ALIGN_BASE);

  if (((alignment < 10) && (alignment != 0)) || (alignment > 32)) {
    return ZX_ERR_INVALID_ARGS;
  }

  *vmar_flags = vmar;
  *arch_mmu_flags |= mmu_flags;
  *align_pow2 = alignment;
  return ZX_OK;
}

}  // namespace

zx_status_t VmoMapper::CreateAndMap(uint64_t size, zx_vm_option_t map_flags,
                                    fbl::RefPtr<VmarManager> vmar_manager,
                                    KernelHandle<VmObjectDispatcher>* vmo_out,
                                    zx_rights_t vmo_rights, uint32_t cache_policy,
                                    uint32_t vmo_options) {
  if (size == 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  zx_status_t res = CheckReadyToMap(vmar_manager);
  if (res != ZX_OK) {
    return res;
  }

  uint64_t aligned_size;
  zx_status_t status = VmObject::RoundSize(size, &aligned_size);
  if (status != ZX_OK) {
    return status;
  }

  fbl::RefPtr<VmObjectPaged> vmo;
  zx_status_t ret = VmObjectPaged::Create(PMM_ALLOC_FLAG_ANY, vmo_options, aligned_size, &vmo);
  if (ret != ZX_OK) {
    return ret;
  }

  if (cache_policy != 0) {
    // ret = vmo->set_cache_policy(cache_policy);
    if (ret != ZX_OK) {
      return ret;
    }
  }

  DEBUG_ASSERT(IS_PAGE_ALIGNED(size));
  status = vmo->CommitRangePinned(0, size, true);
  if (status != ZX_OK) {
    // LTRACEF("vmo->CommitRange failed: %d\n", status);
    return status;
  }

  zx_rights_t rights;
  KernelHandle<VmObjectDispatcher> handle;
  status = VmObjectDispatcher::Create(vmo, size, VmObjectDispatcher::InitialMutability::kMutable,
                                      &handle, &rights);
  if (status != ZX_OK) {
    return ret;
  }

  ret = InternalMap(handle.dispatcher(), 0, size, map_flags, std::move(vmar_manager));
  if (ret != ZX_OK) {
    return ret;
  }

  if (vmo_out) {
    if (vmo_rights != ZX_RIGHT_SAME_RIGHTS) {
      // ret = vmo->Replace(vmo_rights, &vmo);
      if (ret != ZX_OK) {
        Unmap();
        return ret;
      }
    }

    *vmo_out = std::move(handle);
  }

  return ZX_OK;
}

zx_status_t VmoMapper::Map(const fbl::RefPtr<VmObjectDispatcher>& vmo, uint64_t offset,
                           uint64_t size, zx_vm_option_t map_options,
                           fbl::RefPtr<VmarManager> vmar_manager) {
  zx_status_t res;

  if (!vmo) {
    return ZX_ERR_INVALID_ARGS;
  }

  res = CheckReadyToMap(vmar_manager);
  if (res != ZX_OK) {
    return res;
  }

  uint64_t vmo_size;
  res = vmo->GetSize(&vmo_size);
  if (res != ZX_OK) {
    return res;
  }

  uint64_t end_addr;
  if (add_overflow(size, offset, &end_addr) || end_addr > vmo_size) {
    return ZX_ERR_OUT_OF_RANGE;
  }

  if (!size) {
    size = vmo_size - offset;
  }

  return InternalMap(vmo, offset, size, map_options, vmar_manager);
}

void VmoMapper::Unmap() {
  if (start() != nullptr) {
    ZX_DEBUG_ASSERT(size_ != 0);
    mapping_->Destroy();
  }

  vmar_manager_.reset();
  start_ = 0;
  size_ = 0;
}

zx_status_t VmoMapper::CheckReadyToMap(const fbl::RefPtr<VmarManager>& vmar_manager) {
  if (start_ != 0) {
    return ZX_ERR_BAD_STATE;
  }

  if ((vmar_manager != nullptr) && !vmar_manager->vmar()) {
    return ZX_ERR_INVALID_ARGS;
  }

  return ZX_OK;
}

zx_status_t VmoMapper::InternalMap(const fbl::RefPtr<VmObjectDispatcher>& vmo, uint64_t offset,
                                   uint64_t size, zx_vm_option_t map_options,
                                   fbl::RefPtr<VmarManager> vmar_manager) {
  ZX_DEBUG_ASSERT(vmo.get());
  ZX_DEBUG_ASSERT(start() == nullptr);
  ZX_DEBUG_ASSERT(size_ == 0);
  ZX_DEBUG_ASSERT(vmar_manager_ == nullptr);

  if ((map_options & ZX_VM_PERM_READ_IF_XOM_UNSUPPORTED)) {
    if (!(arch_vm_features() & ZX_VM_FEATURE_CAN_MAP_XOM)) {
      map_options |= ZX_VM_PERM_READ;
    }
  }

  if (!VmAddressRegionDispatcher::is_valid_mapping_protection(map_options)) {
    return ZX_ERR_INVALID_ARGS;
  }

  // bool do_map_range = false;
  // if (map_options & ZX_VM_MAP_RANGE) {
  //   do_map_range = true;
  //   map_options &= ~ZX_VM_MAP_RANGE;
  // }

  // if (do_map_range && (map_options & ZX_VM_SPECIFIC_OVERWRITE)) {
  //   return ZX_ERR_INVALID_ARGS;
  // }

  // Usermode is not allowed to specify these flags on mappings, though we may
  // set them below.
  if (map_options & (ZX_VM_CAN_MAP_READ | ZX_VM_CAN_MAP_WRITE | ZX_VM_CAN_MAP_EXECUTE)) {
    return ZX_ERR_INVALID_ARGS;
  }

  // Permissions allowed by both the VMO and the VMAR.
  const bool can_read = true;
  const bool can_write = true;
  const bool can_exec = true;

  // Test to see if the requested mapping protections are allowed.
  if ((map_options & ZX_VM_PERM_READ) && !can_read) {
    return ZX_ERR_ACCESS_DENIED;
  }
  if ((map_options & ZX_VM_PERM_WRITE) && !can_write) {
    return ZX_ERR_ACCESS_DENIED;
  }
  if ((map_options & ZX_VM_PERM_EXECUTE) && !can_exec) {
    return ZX_ERR_ACCESS_DENIED;
  }

  // If a permission is allowed by both the VMO and the VMAR, add it to the
  // flags for the new mapping, so that the VMO's rights as of now can be used
  // to constrain future permission changes via Protect().
  if (can_read) {
    map_options |= ZX_VM_CAN_MAP_READ;
  }
  if (can_write) {
    map_options |= ZX_VM_CAN_MAP_WRITE;
  }
  if (can_exec) {
    map_options |= ZX_VM_CAN_MAP_EXECUTE;
  }

  // Split flags into vmar_flags and arch_mmu_flags
  uint32_t vmar_flags = 0;
  uint arch_mmu_flags = 0;
  uint8_t alignment = 0;
  zx_status_t status = split_syscall_flags(map_options, &vmar_flags, &arch_mmu_flags, &alignment);
  if (status != ZX_OK) {
    return status;
  }

  if (vmar_flags & VMAR_FLAG_REQUIRE_NON_RESIZABLE) {
    vmar_flags &= ~VMAR_FLAG_REQUIRE_NON_RESIZABLE;
    if (vmo->vmo()->is_resizable()) {
      return ZX_ERR_NOT_SUPPORTED;
    }
  }

  if (vmar_flags & VMAR_FLAG_ALLOW_FAULTS) {
    vmar_flags &= ~VMAR_FLAG_ALLOW_FAULTS;
  } else {
    // TODO(https://fxbug.dev/42109795): Add additional checks once all clients (resizable and
    // pager-backed VMOs) start using the VMAR_FLAG_ALLOW_FAULTS flag.
    if (vmo->vmo()->is_discardable()) {
      return ZX_ERR_NOT_SUPPORTED;
    }
  }

  fbl::RefPtr<VmAddressRegion> vmar =
      (vmar_manager == nullptr) ? VmAspace::kernel_aspace()->RootVmar() : vmar_manager->vmar();

  zx::result<VmAddressRegionDispatcher::MapResult> map_result = vmar->CreateVmMapping(
      0, size, alignment, vmar_flags, vmo->vmo(), offset, arch_mmu_flags, "vmo-mapper");

  if (map_result.is_error()) {
    return map_result.status_value();
  }

  // Setup a handler to destroy the new mapping if the syscall is unsuccessful.
  auto cleanup_handler = fit::defer([&map_result]() { map_result->mapping->Destroy(); });

  if (status = map_result->mapping->MapRange(0, size, true); status != ZX_OK) {
    return status;
  }

  cleanup_handler.cancel();

  start_ = map_result->base;
  size_ = size;
  vmar_manager_ = std::move(vmar_manager);
  mapping_ = ktl::move(map_result->mapping);
  return ZX_OK;
}

}  // namespace fzl
