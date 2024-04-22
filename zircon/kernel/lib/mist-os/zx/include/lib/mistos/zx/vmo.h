// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MIST_OS_ZX_INCLUDE_LIB_MISTOS_ZX_VMO_H_
#define ZIRCON_KERNEL_LIB_MIST_OS_ZX_INCLUDE_LIB_MISTOS_ZX_VMO_H_

#include <lib/mistos/zx/handle.h>
#include <lib/mistos/zx/object.h>
#include <lib/mistos/zx/resource.h>
#include <zircon/errors.h>

#include <fbl/ref_ptr.h>
#include <vm/content_size_manager.h>
#include <vm/vm_object.h>
#include <vm/vm_object_paged.h>

namespace zx {

struct VmoStorage : fbl::RefCountedUpgradeable<VmoStorage> {
  VmoStorage(const fbl::RefPtr<VmObject>& vmo,
             const fbl::RefPtr<ContentSizeManager>& content_size_mgr)
      : vmo(vmo), content_size_mgr(content_size_mgr) {}

  fbl::RefPtr<VmObject> vmo;

  // Manages the content size associated with this VMO. The content size is used by streams created
  // against this VMO.
  fbl::RefPtr<ContentSizeManager> content_size_mgr;
};

class vmo final : public object<vmo> {
 public:
  static constexpr zx_obj_type_t TYPE = ZX_OBJ_TYPE_VMO;

  constexpr vmo() = default;

  explicit vmo(fbl::RefPtr<VmoStorage> value) : object(value) {}

  vmo(vmo&& other) : object(other.release()) {}

  vmo& operator=(vmo&& other) {
    reset(other.release());
    return *this;
  }

  static zx_status_t create(uint64_t size, uint32_t options, vmo* result);

  zx_status_t read(void* data, uint64_t offset, size_t len) const;

  zx_status_t write(const void* data, uint64_t offset, size_t len) const;

  zx_status_t transfer_data(uint32_t options, uint64_t offset, uint64_t length, vmo* src_vmo,
                            uint64_t src_offset) {
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t get_size(uint64_t* size) const {
    const fbl::RefPtr<VmoStorage>& tmp = get();
    if (!tmp) {
      return ZX_ERR_BAD_HANDLE;
    }
    *size = tmp->vmo->size();
    return ZX_OK;
  }

  zx_status_t set_size(uint64_t size) const {
    const fbl::RefPtr<VmoStorage>& tmp = get();
    if (!tmp) {
      return ZX_ERR_BAD_HANDLE;
    }
    return tmp->vmo->Resize(size);
  }

  zx_status_t set_prop_content_size(uint64_t size) const {
    return set_property(ZX_PROP_VMO_CONTENT_SIZE, &size, sizeof(size));
  }

  zx_status_t get_prop_content_size(uint64_t* size) const {
    return get_property(ZX_PROP_VMO_CONTENT_SIZE, size, sizeof(*size));
  }

  zx_status_t create_child(uint32_t options, uint64_t offset, uint64_t size, vmo* result) const;

  zx_status_t op_range(uint32_t op, uint64_t offset, uint64_t size, void* buffer,
                       size_t buffer_size) const {
    return range_op(op, offset, size, buffer, buffer_size);
  }

  zx_status_t set_cache_policy(uint32_t cache_policy) const { return ZX_ERR_NOT_SUPPORTED; }

  zx_status_t get_info(uint32_t topic, void* buffer, size_t buffer_size, size_t* actual_count,
                       size_t* avail_count) const final;

  zx_status_t get_property(uint32_t property, void* value, size_t size) const final;

  zx_status_t set_property(uint32_t property, const void* value, size_t size) const final;

  zx_status_t replace_as_executable(const resource& vmex, vmo* result) {
    // LTRACEF("repexec %p\n", get().get());
#if 0
    zx_handle_t h = ZX_HANDLE_INVALID;
    zx_status_t status = zx_vmo_replace_as_executable(value_, vmex.get(), &h);
    // We store ZX_HANDLE_INVALID to value_ before calling reset on result
    // in case result == this.
    value_ = ZX_HANDLE_INVALID;
    result->reset(h);
    return status;
#endif
    return ZX_OK;
  }

 private:
  zx_info_vmo_t get_vmo_info(zx_rights_t rights) const;
  zx_status_t set_content_size(uint64_t content_size) const;
  zx_status_t create_child_internal(uint32_t options, uint64_t offset, uint64_t size,
                                    bool copy_name, fbl::RefPtr<VmObject>* child_vmo) const;
  zx_status_t range_op(uint32_t op, uint64_t offset, uint64_t size, void* buffer,
                       size_t buffer_size) const;
};

using unowned_vmo = unowned<vmo>;

template <>
bool operator<(const unowned<vmo>& a, const unowned<vmo>& b);

enum class VmoOwnership { kHandle, kMapping, kIoBuffer };
zx_info_vmo_t VmoToInfoEntry(const VmObject* vmo, VmoOwnership ownership,
                             zx_rights_t handle_rights);

}  // namespace zx

#endif  // ZIRCON_KERNEL_LIB_MIST_OS_ZX_INCLUDE_LIB_MISTOS_ZX_VMO_H_
