// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MIST_OS_ZX_INCLUDE_LIB_MISTOS_ZX_VMAR_H_
#define ZIRCON_KERNEL_LIB_MIST_OS_ZX_INCLUDE_LIB_MISTOS_ZX_VMAR_H_

#include <lib/fit/defer.h>
#include <lib/mistos/util/process.h>
#include <lib/mistos/zx/object.h>
#include <lib/mistos/zx/vmo.h>
#include <zircon/features.h>

#include <fbl/ref_ptr.h>
#include <vm/vm_address_region.h>

namespace zx {

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

}  // namespace

zx_status_t split_flags(uint32_t flags, uint32_t* vmar_flags, uint* arch_mmu_flags,
                        uint8_t* align_pow2);
bool is_valid_mapping_protection(uint32_t flags);

// A wrapper for handles to VMARs.  Note that vmar::~vmar() does not execute
// vmar::destroy(), it just closes the handle.
class vmar final : public object<vmar> {
 public:
  static constexpr zx_obj_type_t TYPE = ZX_OBJ_TYPE_VMAR;

  constexpr vmar() = default;

  explicit vmar(fbl::RefPtr<VmAddressRegion> value) : object(value) {}

  vmar(vmar&& other) : object(other.release()) {}

  vmar& operator=(vmar&& other) {
    reset(other.release());
    return *this;
  }

  zx_status_t map(zx_vm_option_t options, size_t vmar_offset, const vmo& vmo, uint64_t vmo_offset,
                  size_t len, zx_vaddr_t* ptr, bool is_user = false) const;

  zx_status_t unmap(uintptr_t address, size_t len) const {
    if (!get()) {
      return ZX_ERR_BAD_HANDLE;
    }
    return get()->Unmap(address, len, VmAddressRegionOpChildren::Yes);
  }

  zx_status_t protect(zx_vm_option_t options, uintptr_t address, size_t len,
                      bool is_user = false) const;

  zx_status_t op_range(uint32_t op, uint64_t offset, uint64_t size, void* buffer,
                       size_t buffer_size) const {
    // return zx_vmar_op_range(get(), op, offset, size, buffer, buffer_size);
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t destroy() const {
    if (!get()) {
      return ZX_ERR_BAD_HANDLE;
    }
    return get()->Destroy();
  }

  zx_status_t allocate(uint32_t options, size_t offset, size_t size, vmar* child,
                       uintptr_t* child_addr, bool is_user = false) const;

  static inline unowned<vmar> root_self() { return unowned<vmar>(zx_vmar_root_self()); }

  zx_status_t get_info(uint32_t topic, void* buffer, size_t buffer_size, size_t* actual_count,
                       size_t* avail_count) const final;
};

using unowned_vmar = unowned<vmar>;

}  // namespace zx

#endif  // ZIRCON_KERNEL_LIB_MIST_OS_ZX_INCLUDE_LIB_MISTOS_ZX_VMAR_H_
