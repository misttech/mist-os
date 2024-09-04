// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/testing/testing.h"

#include <lib/mistos/starnix/kernel/fs/tmpfs.h>
#include <lib/mistos/starnix/kernel/mm/flags.h>
#include <lib/mistos/starnix/kernel/mm/syscalls.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/fs_context.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>

#include <fbl/ref_ptr.h>

using namespace starnix;

namespace {

// Creates a `Kernel`, `Task`, and `Locked<Unlocked>` for testing purposes.
//
// The `Task` is backed by a real process, and can be used to test syscalls.
ktl::pair<fbl::RefPtr<Kernel>, starnix::testing::AutoReleasableTask>
create_kernel_task_and_unlocked_with_fs_and_selinux(
    std::function<FileSystemHandle(const fbl::RefPtr<Kernel>&)> create_fs/*,
    security_server: Option<Arc<SecurityServer>>*/) {
  fbl::RefPtr<Kernel> kernel = Kernel::New("").value_or(fbl::RefPtr<Kernel>());
  ASSERT_MSG(kernel, "failed to create kernel");

  auto init_pid = kernel->pids.Write()->allocate_pid();
  ASSERT(init_pid == 1);
  auto fs = FsContext::New(create_fs(kernel));
  auto init_task = CurrentTask::create_init_process(kernel, init_pid, "test-task", fs);

  return {ktl::move(kernel), testing::AutoReleasableTask::From(init_task.value())};
}

}  // namespace

namespace starnix::testing {

/// TODO (Herrera) fix to open Bootfs from ZBI
/// Create a FileSystemHandle for use in testing.
///
/// Open "/pkg" and returns an FsContext rooted in that directory.
FileSystemHandle create_pkgfs(const fbl::RefPtr<Kernel>& kernel) {
  // For now use a TmpFs
  return TmpFs::new_fs(kernel);
}

/// Creates a `Kernel`, `Task`, and `Locked<Unlocked>` with the package file system for testing
/// purposes.
///
/// The `Task` is backed by a real process, and can be used to test syscalls.
ktl::pair<fbl::RefPtr<Kernel>, starnix::testing::AutoReleasableTask>
create_kernel_task_and_unlocked_with_pkgfs() {
  return create_kernel_task_and_unlocked_with_fs_and_selinux(create_pkgfs);
}

ktl::pair<fbl::RefPtr<Kernel>, starnix::testing::AutoReleasableTask>
create_kernel_task_and_unlocked() {
  return create_kernel_task_and_unlocked_with_fs_and_selinux(
      [](const fbl::RefPtr<Kernel>& kernel) -> FileSystemHandle { return TmpFs::new_fs(kernel); });
}

ktl::pair<fbl::RefPtr<Kernel>, AutoReleasableTask> create_kernel_and_task() {
  return create_kernel_task_and_unlocked();
}

AutoReleasableTask create_task(fbl::RefPtr<Kernel>& kernel, const ktl::string_view& task_name) {
  auto init_task = CurrentTask::create_init_child_process(kernel, task_name);
  return ktl::move(testing::AutoReleasableTask::From(init_task.value()));
}

UserAddress map_memory(CurrentTask& current_task, UserAddress address, uint64_t length) {
  return map_memory_with_flags(current_task, address, length, MAP_ANONYMOUS | MAP_PRIVATE);
}

UserAddress map_memory_with_flags(CurrentTask& current_task, UserAddress address, uint64_t length,
                                  uint32_t flags) {
  auto result = do_mmap(current_task, address, length, PROT_READ | PROT_WRITE, flags,
                        FdNumber::from_raw(-1), 0);
  ASSERT_MSG(result.is_ok(), "Could not map memory");
  return result.value();
}

}  // namespace starnix::testing
