// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_VMO_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_VMO_H_

#include <lib/mistos/zx/handle.h>
#include <lib/mistos/zx/object.h>
#include <lib/mistos/zx/resource.h>

namespace zx {

class vmo final : public object<vmo> {
 public:
  static constexpr zx_obj_type_t TYPE = ZX_OBJ_TYPE_VMO;

  constexpr vmo() = default;

  explicit vmo(fbl::RefPtr<Value> handle) : object(handle) {}

  explicit vmo(handle&& h) : object(h.release()) {}

  vmo(vmo&& other) : object(other.release()) {}

  vmo& operator=(vmo&& other) {
    reset(other.release());
    return *this;
  }

  static zx_status_t create(uint64_t size, uint32_t options, vmo* result);

  zx_status_t read(void* data, uint64_t offset, size_t len) const;

  zx_status_t write(const void* data, uint64_t offset, size_t len) const;

  zx_status_t transfer_data(uint32_t options, uint64_t offset, uint64_t length, vmo* src_vmo,
                            uint64_t src_offset);

  zx_status_t get_size(uint64_t* size) const;

  zx_status_t set_size(uint64_t size) const;

  zx_status_t set_prop_content_size(uint64_t size) const {
    return set_property(ZX_PROP_VMO_CONTENT_SIZE, &size, sizeof(size));
  }

  zx_status_t get_prop_content_size(uint64_t* size) const {
    return get_property(ZX_PROP_VMO_CONTENT_SIZE, size, sizeof(*size));
  }

  zx_status_t create_child(uint32_t options, uint64_t offset, uint64_t size, vmo* result) const;

  zx_status_t op_range(uint32_t op, uint64_t offset, uint64_t size, void* buffer,
                       size_t buffer_size) const;

  zx_status_t set_cache_policy(uint32_t cache_policy) const { return ZX_ERR_NOT_SUPPORTED; }

  zx_status_t replace_as_executable(const resource& vmex, vmo* result);
};

using unowned_vmo = unowned<vmo>;

}  // namespace zx

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_VMO_H_
