// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_ZIRCON_TASK_DISPATCHER_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_ZIRCON_TASK_DISPATCHER_H_

#include <zircon/types.h>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <object/dispatcher.h>

namespace starnix {
class Task;
}

class TaskDispatcher final : public SoloDispatcher<TaskDispatcher, ZX_DEFAULT_STARNIX_TASK_RIGHTS> {
 public:
  static zx_status_t Create(fbl::RefPtr<starnix::Task> task,
                            KernelHandle<TaskDispatcher>* out_handle, zx_rights_t* out_rights);

  ~TaskDispatcher() final;

  // Dispatcher implementation.
  zx_obj_type_t get_type() const final { return ZX_OBJ_TYPE_STRANIX_TASK; }

  fbl::RefPtr<starnix::Task> task() const;

 private:
  explicit TaskDispatcher(fbl::RefPtr<starnix::Task> task);

  fbl::RefPtr<starnix::Task> task_;
};

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_ZIRCON_TASK_DISPATCHER_H_
