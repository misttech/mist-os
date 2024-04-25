// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/zx/vmar.h"

#include <trace.h>

#include "zx_priv.h"

#define LOCAL_TRACE ZX_GLOBAL_TRACE(0)

namespace zx {

// Split out the syscall flags into vmar flags and mmu flags.  Note that this
// does not validate that the requested protections in *flags* are valid.  For
// that use is_valid_mapping_protection()
zx_status_t split_flags(uint32_t flags, uint32_t* vmar_flags, uint* arch_mmu_flags,
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

bool is_valid_mapping_protection(uint32_t flags) {
  if (!(flags & ZX_VM_PERM_READ)) {
    // No way to express non-readable mappings that are also writeable or
    // executable.
    if (flags & (ZX_VM_PERM_WRITE | ZX_VM_PERM_EXECUTE)) {
      return false;
    }
  }
  return true;
}

zx_status_t vmar::map(zx_vm_option_t options, size_t vmar_offset, const vmo& vmo,
                      uint64_t vmo_offset, size_t len, zx_vaddr_t* ptr, bool is_user) const {
  LTRACE;
  if (!get() || !vmo.is_valid()) {
    return ZX_ERR_BAD_HANDLE;
  }

  if ((options & ZX_VM_PERM_READ_IF_XOM_UNSUPPORTED)) {
    if (!(arch_vm_features() & ZX_VM_FEATURE_CAN_MAP_XOM)) {
      options |= ZX_VM_PERM_READ;
    }
  }

  if (!is_valid_mapping_protection(options)) {
    return ZX_ERR_INVALID_ARGS;
  }

  bool do_map_range = false;
  if (options & ZX_VM_MAP_RANGE) {
    do_map_range = true;
    options &= ~ZX_VM_MAP_RANGE;
  }

  if (do_map_range && (options & ZX_VM_SPECIFIC_OVERWRITE)) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (is_user) {
    // Usermode is not allowed to specify these flags on mappings, though we may
    // set them below.
    if (options & (ZX_VM_CAN_MAP_READ | ZX_VM_CAN_MAP_WRITE | ZX_VM_CAN_MAP_EXECUTE)) {
      return ZX_ERR_INVALID_ARGS;
    }
  }

  if ((options & ZX_VM_PERM_READ)) {
    options |= ZX_VM_CAN_MAP_READ;
  }
  if ((options & ZX_VM_PERM_WRITE)) {
    options |= ZX_VM_CAN_MAP_WRITE;
  }
  if ((options & ZX_VM_PERM_EXECUTE)) {
    options |= ZX_VM_CAN_MAP_EXECUTE;
  }

  // Split flags into vmar_flags and arch_mmu_flags
  uint32_t vmar_flags = 0;
  uint arch_mmu_flags = is_user ? ARCH_MMU_FLAG_PERM_USER : 0;
  uint8_t alignment = 0;
  zx_status_t status = split_flags(options, &vmar_flags, &arch_mmu_flags, &alignment);
  if (status != ZX_OK) {
    return status;
  }

  if (vmar_flags & VMAR_FLAG_REQUIRE_NON_RESIZABLE) {
    vmar_flags &= ~VMAR_FLAG_REQUIRE_NON_RESIZABLE;
    if (vmo.get()->vmo()->is_resizable()) {
      return ZX_ERR_NOT_SUPPORTED;
    }
  }

  if (vmar_flags & VMAR_FLAG_ALLOW_FAULTS) {
    vmar_flags &= ~VMAR_FLAG_ALLOW_FAULTS;
  } else {
    // TODO(https://fxbug.dev/42109795): Add additional checks once all clients (resizable and
    // pager-backed VMOs) start using the VMAR_FLAG_ALLOW_FAULTS flag.
    if (vmo.get()->vmo()->is_discardable()) {
      return ZX_ERR_NOT_SUPPORTED;
    }
  }

  zx::result<VmAddressRegion::MapResult> map_result = get()->CreateVmMapping(
      vmar_offset, len, alignment,
      vmar_flags | (is_user ? 0 : VMAR_FLAG_DEBUG_DYNAMIC_KERNEL_MAPPING), vmo.get()->vmo(),
      vmo_offset, arch_mmu_flags, is_user ? "useralloc" : "kernelalloc");

  if (map_result.is_error()) {
    return map_result.status_value();
  }

  // Setup a handler to destroy the new mapping if the syscall is unsuccessful.
  auto cleanup_handler = fit::defer([&map_result]() { map_result->mapping->Destroy(); });

  if (is_user) {
    if (do_map_range) {
      // Mappings may have already been created due to memory priority, so need to ignore existing.
      // Ignoring existing mappings is safe here as we are always free to populate and destroy page
      // table mappings for user addresses.
      status = map_result->mapping->MapRange(0, len, /*commit=*/false, /*ignore_existing=*/true);
      if (status != ZX_OK) {
        return status;
      }
    }
  } else {
    status = map_result->mapping->MapRange(0, len, /*commit=*/true);
    if (status != ZX_OK) {
      return status;
    }
  }

  if (ptr) {
    *ptr = map_result->base;
  }

  cleanup_handler.cancel();

  // This mapping will now always be used via the aspace so it is free to be merged into different
  // actual mapping objects.
  VmMapping::MarkMergeable(ktl::move(map_result->mapping));

  return ZX_OK;
}

zx_status_t vmar::protect(zx_vm_option_t options, uintptr_t address, size_t len,
                          bool is_user) const {
  LTRACE;
  if ((options & ZX_VM_PERM_READ_IF_XOM_UNSUPPORTED)) {
    if (!(arch_vm_features() & ZX_VM_FEATURE_CAN_MAP_XOM)) {
      options |= ZX_VM_PERM_READ;
    }
  }

  if (!IS_PAGE_ALIGNED(address)) {
    return ZX_ERR_INVALID_ARGS;
  }

  if (!is_valid_mapping_protection(options)) {
    return ZX_ERR_INVALID_ARGS;
  }

  uint32_t vmar_flags = 0;
  uint arch_mmu_flags = is_user ? ARCH_MMU_FLAG_PERM_USER : 0;
  uint8_t alignment = 0;
  zx_status_t status = split_flags(options, &vmar_flags, &arch_mmu_flags, &alignment);
  if (status != ZX_OK)
    return status;

  // This request does not allow any VMAR flags or alignment flags to be set.
  if (vmar_flags || (alignment != 0))
    return ZX_ERR_INVALID_ARGS;

  return get()->Protect(address, len, arch_mmu_flags, VmAddressRegionOpChildren::Yes);
}

zx_status_t vmar::allocate(uint32_t options, size_t offset, size_t size, vmar* child,
                           uintptr_t* child_addr, bool is_user) const {
  LTRACE;
  if (!get()) {
    return ZX_ERR_BAD_HANDLE;
  }

  uint32_t vmar_flags = 0;
  uint arch_mmu_flags = 0;
  uint8_t alignment = 0;
  zx_status_t status = split_flags(options, &vmar_flags, &arch_mmu_flags, &alignment);
  if (status != ZX_OK)
    return status;

  // Check if any MMU-related flags were requested.
  if (arch_mmu_flags != 0) {
    return ZX_ERR_INVALID_ARGS;
  }

  fbl::RefPtr<VmAddressRegion> new_vmar;
  status = get()->CreateSubVmar(offset, size, alignment, vmar_flags,
                                is_user ? "useralloc" : "kernelalloc", &new_vmar);
  if (status != ZX_OK)
    return status;

  *child_addr = new_vmar->base();

  child->reset(std::move(new_vmar));
  return ZX_OK;
}

zx_status_t vmar::get_info(uint32_t topic, void* buffer, size_t buffer_size, size_t* actual_count,
                           size_t* avail_count) const {
  LTRACE;
  if (!get()) {
    return ZX_ERR_BAD_HANDLE;
  }

  switch (topic) {
    case ZX_INFO_VMAR: {
      fbl::RefPtr<VmAddressRegion> vmar = get();
      zx_info_vmar_t info = {
          .base = vmar->base(),
          .len = vmar->size(),
      };
      return single_record_result(buffer, buffer_size, actual_count, avail_count, info);
    }
    default:
      LTRACEF("[NOT_SUPPORTED] topic %d\n", topic);
      return ZX_ERR_NOT_SUPPORTED;
  }
}

}  // namespace zx
