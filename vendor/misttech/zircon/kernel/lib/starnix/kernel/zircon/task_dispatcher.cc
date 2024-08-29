// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/zircon/task_dispatcher.h"

#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>

#include <fbl/ref_ptr.h>

TaskDispatcher::~TaskDispatcher() {}

zx_status_t TaskDispatcher::Create(fbl::RefPtr<starnix::Task> task,
                                   KernelHandle<TaskDispatcher>* out_handle,
                                   zx_rights_t* out_rights) {
  fbl::AllocChecker ac;
  KernelHandle new_handle(fbl::AdoptRef(new (&ac) TaskDispatcher(ktl::move(task))));
  if (!ac.check()) {
    return ZX_ERR_NO_MEMORY;
  }
  *out_rights = default_rights();
  *out_handle = ktl::move(new_handle);
  return ZX_OK;
}

TaskDispatcher::TaskDispatcher(fbl::RefPtr<starnix::Task> task)
    : SoloDispatcher(), task_(ktl::move(task)) {}

fbl::RefPtr<starnix::Task> TaskDispatcher::task() const {
  ASSERT(task_);
  return task_;
}
