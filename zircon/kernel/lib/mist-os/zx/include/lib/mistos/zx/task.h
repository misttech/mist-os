// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MIST_OS_ZX_INCLUDE_LIB_MISTOS_ZX_TASK_H_
#define ZIRCON_KERNEL_LIB_MIST_OS_ZX_INCLUDE_LIB_MISTOS_ZX_TASK_H_

#include <lib/mistos/zx/handle.h>
#include <lib/mistos/zx/object.h>
#include <lib/mistos/zx/object_traits.h>
#include <lib/mistos/zx/suspend_token.h>

namespace zx {

class suspend_token;

template <typename T = raw_ptr_t>
class task : public object<T> {
 public:
  constexpr task() = default;

  explicit task(typename object<T>::StorageType value) : object<T>(value) {}

  task(task&& other) : object<T>(other.release()) {}

  zx_status_t kill() const {
    static_assert(object_traits<T>::supports_kill, "Object must support being killed.");
    // return zx_task_kill(object<T>::get());
    return ZX_ERR_NOT_SUPPORTED;
  }

  zx_status_t suspend(suspend_token* result) const {
    // Assume |result| must refer to a different container than |this|, due
    // to strict aliasing.
    // return zx_task_suspend_token(object<T>::get(), result->reset_and_get_address());
    return ZX_ERR_NOT_SUPPORTED;
  }

  // zx_status_t create_exception_channel(uint32_t options, object<channel>* channel) const {
  //   return ZX_ERR_NOT_SUPPORTED;
  // }
};

}  // namespace zx

#endif  // ZIRCON_KERNEL_LIB_MIST_OS_ZX_INCLUDE_LIB_MISTOS_ZX_TASK_H_
