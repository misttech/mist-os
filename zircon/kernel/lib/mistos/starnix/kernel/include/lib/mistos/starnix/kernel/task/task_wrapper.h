// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_TASK_WRAPPER_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_TASK_WRAPPER_H_

#include <lib/mistos/starnix/kernel/task/forward.h>

#include <fbl/ref_counted.h>
#include <fbl/ref_ptr.h>

namespace starnix {

class TaskWrapper : public fbl::RefCounted<TaskWrapper> {
 public:
  TaskWrapper(fbl::RefPtr<Task> task);
  ~TaskWrapper();

  fbl::RefPtr<Task> task() const;

 private:
  fbl::RefPtr<Task> task_;
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_TASK_TASK_WRAPPER_H_
