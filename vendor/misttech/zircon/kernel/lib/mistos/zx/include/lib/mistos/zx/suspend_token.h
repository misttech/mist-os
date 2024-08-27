// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_SUSPEND_TOKEN_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_SUSPEND_TOKEN_H_

#include <lib/mistos/zx/handle.h>
#include <lib/mistos/zx/object.h>

namespace zx {

// The only thing you can do with a suspend token is close it (which will
// resume the thread).
class suspend_token final : public object<suspend_token> {
 public:
  static constexpr zx_obj_type_t TYPE = ZX_OBJ_TYPE_SUSPEND_TOKEN;

  constexpr suspend_token() = default;

  explicit suspend_token(zx_handle_t value) : object<suspend_token>(value) {}

  explicit suspend_token(handle&& h) : object<suspend_token>(h.release()) {}

  suspend_token(suspend_token&& other) : object<suspend_token>(other.release()) {}

  suspend_token& operator=(suspend_token&& other) {
    reset(other.release());
    return *this;
  }
};

using unowned_suspend_token = unowned<suspend_token>;

}  // namespace zx

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_SUSPEND_TOKEN_H_
