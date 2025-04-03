// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fzl/vmar-manager.h>

#include <utility>

#include <fbl/alloc_checker.h>

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

fbl::RefPtr<VmarManager> VmarManager::Create(size_t size, fbl::RefPtr<VmarManager> parent,
                                             zx_vm_option_t options) {
  if (!size || (parent && !parent->vmar())) {
    return nullptr;
  }

  fbl::AllocChecker ac;
  fbl::RefPtr<VmarManager> ret = fbl::AdoptRef(new (&ac) VmarManager());

  if (!ac.check()) {
    return nullptr;
  }

  uint32_t vmar_flags = 0;
  uint arch_mmu_flags = 0;
  uint8_t alignment = 0;
  zx_status_t status = split_syscall_flags(options, &vmar_flags, &arch_mmu_flags, &alignment);
  if (status != ZX_OK) {
    return nullptr;
  }

  // Check if any MMU-related flags were requested.
  if (arch_mmu_flags != 0) {
    return nullptr;
  }

  zx_status_t res;
  uintptr_t child_addr;
  if (parent) {
    res = parent->vmar()->CreateSubVmar(0, size, alignment, vmar_flags, "kernel-child-vmar",
                                        &ret->vmar_);
  } else {
    res = VmAspace::kernel_aspace()->RootVmar()->CreateSubVmar(0, size, alignment, vmar_flags,
                                                               "kernel-root-vmar", &ret->vmar_);
  }

  if (res != ZX_OK) {
    return nullptr;
  }

  child_addr = ret->vmar_->base();

  ret->parent_ = std::move(parent);
  ret->start_ = reinterpret_cast<void*>(child_addr);
  ret->size_ = size;

  return ret;
}

}  // namespace fzl
