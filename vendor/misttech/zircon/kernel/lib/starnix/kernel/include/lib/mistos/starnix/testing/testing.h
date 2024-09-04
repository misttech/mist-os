// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_TESTING_TESTING_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_TESTING_TESTING_H_

#include <lib/mistos/starnix/kernel/task/current_task.h>

#include <fbl/ref_ptr.h>
#include <ktl/optional.h>
#include <ktl/pair.h>
#include <ktl/string_view.h>

namespace starnix {

class Kernel;

namespace testing {

class AutoReleasableTask {
 public:
  AutoReleasableTask() = default;

  static AutoReleasableTask From(const starnix::TaskBuilder& builder) {
    return AutoReleasableTask::From(starnix::CurrentTask::From(builder));
  }

  static AutoReleasableTask From(const starnix::CurrentTask& task) {
    return AutoReleasableTask(task);
  }

  starnix::CurrentTask& operator*() {
    ASSERT_MSG(task_.has_value(),
               "called `operator*` on ktl::optional that does not contain a value.");
    return task_.value();
  }

  starnix::CurrentTask operator->() {
    ASSERT_MSG(task_.has_value(),
               "called `operator->` on ktl::optional that does not contain a value.");
    return task_.value();
  }

 private:
  AutoReleasableTask(ktl::optional<starnix::CurrentTask> task) : task_(ktl::move(task)) {}

  ktl::optional<starnix::CurrentTask> task_;
};

ktl::pair<fbl::RefPtr<Kernel>, starnix::testing::AutoReleasableTask>
create_kernel_task_and_unlocked_with_pkgfs();

ktl::pair<fbl::RefPtr<starnix::Kernel>, AutoReleasableTask> create_kernel_task_and_unlocked();

ktl::pair<fbl::RefPtr<starnix::Kernel>, AutoReleasableTask> create_kernel_and_task();

// Creates a new `Task` in the provided kernel.
//
// The `Task` is backed by a real process, and can be used to test syscalls.
AutoReleasableTask create_task(fbl::RefPtr<starnix::Kernel>& kernel,
                               const ktl::string_view& task_name);

// Maps `length` at `address` with `PROT_READ | PROT_WRITE`, `MAP_ANONYMOUS | MAP_PRIVATE`.
//
// Returns the address returned by `sys_mmap`.
UserAddress map_memory(starnix::CurrentTask& current_task, UserAddress address, uint64_t length);

// Maps `length` at `address` with `PROT_READ | PROT_WRITE` and the specified flags.
//
// Returns the address returned by `sys_mmap`.
UserAddress map_memory_with_flags(starnix::CurrentTask& current_task, UserAddress address,
                                  uint64_t length, uint32_t flags);

// FileSystemHandle create_fs(fbl::RefPtr<starnix::Kernel>& kernel, fbl::RefPtr<FsNodeOps> ops);

}  // namespace testing
}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_TESTING_TESTING_H_
