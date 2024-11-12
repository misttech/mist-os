// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/task/kernel_threads.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>

namespace starnix {

SystemTask::SystemTask(CurrentTask system_task)
    : system_task_(ktl::move(system_task)),
      system_thread_group_((*system_task_)->thread_group_->weak_factory_.GetWeakPtr()) {}

KernelThreads KernelThreads::New() { return KernelThreads(); }

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

void KernelThreads::set_kernel(mtl::WeakPtr<Kernel> kernel) {
  ASSERT(kernel_ == nullptr);
  kernel_ = kernel;
}

}  // namespace starnix
