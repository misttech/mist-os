// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_ZIRCON_INCLUDE_LIB_STARNIX_ZIRCON_TASK_WRAPPER_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_ZIRCON_INCLUDE_LIB_STARNIX_ZIRCON_TASK_WRAPPER_H_

#include <lib/mistos/starnix/kernel/task/current_task.h>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>
#include <ktl/optional.h>

namespace starnix {
class Task;
class CurrentTask;
}  // namespace starnix

class TaskWrapper : public fbl::RefCounted<TaskWrapper> {
 public:
  TaskWrapper(ktl::optional<starnix::CurrentTask> task);
  ~TaskWrapper();

  starnix::CurrentTask& into();

 private:
  ktl::optional<starnix::CurrentTask> task_;
};

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_STARNIX_LIB_STARNIX_ZIRCON_INCLUDE_LIB_STARNIX_ZIRCON_TASK_WRAPPER_H_
