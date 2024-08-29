// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_EXECUTION_SHARED_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_EXECUTION_SHARED_H_

#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/zx/thread.h>

#include <optional>

#include <fbl/ref_ptr.h>

namespace starnix {

/// Result returned when creating new Zircon threads and processes for tasks.
struct TaskInfo {
  // The thread that was created for the task.
  ktl::optional<zx::thread> thread;

  // The thread group that the task should be added to.
  fbl::RefPtr<ThreadGroup> thread_group;

  // The memory manager to use for the task.
  fbl::RefPtr<MemoryManager> memory_manager;
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_EXECUTION_SHARED_H_
