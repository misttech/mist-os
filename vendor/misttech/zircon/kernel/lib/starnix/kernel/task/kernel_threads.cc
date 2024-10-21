// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/task/kernel_threads.h>
#include <lib/mistos/starnix/kernel/task/task.h>

namespace starnix {

SystemTask::SystemTask(CurrentTask system_task)
    : system_task_(ktl::move(system_task)),
      system_thread_group_(util::WeakPtr<ThreadGroup>((*system_task_)->thread_group().get())) {}

KernelThreads KernelThreads::New(util::WeakPtr<Kernel> kernel) { return KernelThreads(kernel); }

KernelThreads::~KernelThreads() {
  auto system_task = system_task_.take();
  if (system_task) {
    system_task->system_task_->release();
  }
}

fit::result<Errno> KernelThreads::Init(CurrentTask system_task) {
  auto was_empty = system_task_.set(SystemTask(ktl::move(system_task)));
  if (!was_empty) {
    return fit::error(errno(EEXIST));
  }
  return fit::ok();
}

CurrentTask& KernelThreads::system_task() { return system_task_.get().system_task_.value(); }

}  // namespace starnix
