// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/starnix_zircon/task_wrapper.h"

#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <trace.h>

#include <ktl/move.h>

#include "../kernel_priv.h"

#define LOCAL_TRACE STARNIX_KERNEL_GLOBAL_TRACE(0)

TaskWrapper::TaskWrapper(ktl::optional<starnix::CurrentTask> task) : task_(ktl::move(task)) {
  LTRACE_ENTRY_OBJ;
}

TaskWrapper::~TaskWrapper() {
  LTRACE_ENTRY_OBJ;
  ZX_ASSERT(task_.has_value());
  task_->release();
  LTRACE_EXIT_OBJ;
}

starnix::CurrentTask& TaskWrapper::into() {
  ZX_ASSERT(task_.has_value());
  return task_.value();
}
