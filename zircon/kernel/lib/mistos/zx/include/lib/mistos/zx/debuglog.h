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

/*
struct LogStorage : fbl::RefCountedUpgradeable<LogStorage> {
  LogStorage() : vmo(vmo), content_size_mgr(content_size_mgr) {}

  mutable DECLARE_CRITICAL_MUTEX(LogStorage, lockdep::LockFlagsNone) lock_;
};
*/

class debuglog final : public object<debuglog> {
 public:
  static constexpr zx_obj_type_t TYPE = ZX_OBJ_TYPE_LOG;

  constexpr debuglog() = default;

  explicit debuglog(raw_ptr_t value) : object(value) {}

  debuglog(debuglog&& other) : object(other.release()) {}

  debuglog& operator=(debuglog&& other) {
    reset(other.release());
    // this.flags_ = other.flags_;
    return *this;
  }

  static zx_status_t create(const resource& resource, uint32_t options, debuglog* result);

  zx_status_t write(uint32_t options, const void* buffer, size_t buffer_size) const;

  zx_status_t read(uint32_t options, void* buffer, size_t buffer_size) const;

 private:
  // const uint32_t flags_;
};

using unowned_debuglog = unowned<debuglog>;

}  // namespace zx

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_DEBUGLOG_H_
