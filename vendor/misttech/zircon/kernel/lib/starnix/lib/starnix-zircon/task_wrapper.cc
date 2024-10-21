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

TaskWrapper::TaskWrapper(fbl::RefPtr<starnix::Task> task) : task_(ktl::move(task)) {}

TaskWrapper::~TaskWrapper() {
  LTRACE_ENTRY_OBJ;

  if (task_) {
    starnix::ThreadState dummy = {};
    task_->release(dummy);
  }
}

starnix::CurrentTask TaskWrapper::into() {
  starnix::TaskBuilder builder(task_);
  return starnix::CurrentTask::From(ktl::move(builder));
}
