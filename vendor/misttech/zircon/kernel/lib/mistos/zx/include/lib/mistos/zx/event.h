// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_EVENT_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_EVENT_H_

#include <lib/mistos/zx/handle.h>
#include <lib/mistos/zx/object.h>
#include <zircon/availability.h>

#include <object/event_dispatcher.h>

namespace zx {

class event final : public object<event> {
 public:
  static constexpr zx_obj_type_t TYPE = ZX_OBJ_TYPE_EVENT;

  constexpr event() = default;

  explicit event(zx_handle_t value) : object(value) {}

  explicit event(handle&& h) : object(h.release()) {}

  event(event&& other) : object(other.release()) {}

  event& operator=(event&& other) {
    reset(other.release());
    return *this;
  }

  static zx_status_t create(uint32_t options, event* result);
};

using unowned_event = unowned<event>;

}  // namespace zx

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZX_INCLUDE_LIB_MISTOS_ZX_EVENT_H_
