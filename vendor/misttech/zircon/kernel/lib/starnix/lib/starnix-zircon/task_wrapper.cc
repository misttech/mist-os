// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/starnix_zircon/task_wrapper.h"

#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/task.h>

#include <ktl/move.h>

TaskWrapper::TaskWrapper(fbl::RefPtr<starnix::Task> task) : task_(ktl::move(task)) {}

TaskWrapper::~TaskWrapper() = default;

starnix::CurrentTask TaskWrapper::into() {
  starnix::TaskBuilder builder(task_);
  return starnix::CurrentTask::From(builder);
}
