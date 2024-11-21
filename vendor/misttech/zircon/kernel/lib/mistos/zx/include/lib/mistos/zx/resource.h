// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_RESOURCE_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_RESOURCE_H_

#include <lib/mistos/zx/handle.h>
#include <lib/mistos/zx/object.h>

namespace zx {

class resource final : public object<resource> {
 public:
  static constexpr zx_obj_type_t TYPE = ZX_OBJ_TYPE_RESOURCE;

  constexpr resource() = default;

  explicit resource(Handle* handle) : object(handle) {}

  explicit resource(handle&& h) : object(h.release()) {}

  resource(resource&& other) : object(other.release()) {}

  resource& operator=(resource&& other) {
    reset(other.release());
    return *this;
  }

  static zx_status_t create(const resource& parent, uint32_t options, uint64_t base, size_t len,
                            const char* name, size_t namelen, resource* result);
};

using unowned_resource = unowned<resource>;

}  // namespace zx

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_RESOURCE_H_
