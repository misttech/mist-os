// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_DEBUGLOG_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_DEBUGLOG_H_

#include <lib/mistos/zx/object.h>
#include <lib/mistos/zx/resource.h>
#include <zircon/errors.h>

namespace zx {

class debuglog final : public object<debuglog> {
 public:
  static constexpr zx_obj_type_t TYPE = ZX_OBJ_TYPE_LOG;

  constexpr debuglog() = default;

  explicit debuglog(zx_handle_t value) : object(value) {}

  explicit debuglog(handle&& h) : object(h.release()) {}

  debuglog(debuglog&& other) : object(other.release()) {}

  debuglog& operator=(debuglog&& other) {
    reset(other.release());
    return *this;
  }

  static zx_status_t create(const resource& resource, uint32_t options, debuglog* result);

  zx_status_t write(uint32_t options, const void* buffer, size_t buffer_size) const {
    return zx_debuglog_write(get(), options, buffer, buffer_size);
  }

  zx_status_t read(uint32_t options, void* buffer, size_t buffer_size) const {
    return zx_debuglog_read(get(), options, buffer, buffer_size);
  }
};

using unowned_debuglog = unowned<debuglog>;

}  // namespace zx

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_DEBUGLOG_H_
