// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/testing/testing.h"

#include <lib/mistos/starnix/kernel/fs/mistos/bootfs.h>
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
#include <lib/mistos/starnix/kernel/vfs/namespace.h>
#include <lib/starnix/bootfs/tests/data/bootfs.zbi.h>
#include <lib/starnix/bootfs/tests/zbi_file.h>

#include <fbl/ref_ptr.h>

using namespace starnix;

namespace starnix::testing {

fbl::RefPtr<Kernel> create_test_kernel(/*security_server: Arc<SecurityServer>*/) {
  return Kernel::New("").value();
}

template <typename CreateFsFn>
fbl::RefPtr<FsContext> create_test_fs_context(const fbl::RefPtr<Kernel>& kernel,
                                              CreateFsFn create_fs) {
  return FsContext::New(Namespace::New(create_fs(kernel)));
}

TaskBuilder create_test_init_task(fbl::RefPtr<Kernel> kernel, fbl::RefPtr<FsContext> fs) {
  auto init_pid = kernel->pids.Write()->allocate_pid();
  ASSERT(init_pid == 1);
  auto init_task = CurrentTask::create_init_process(kernel, init_pid, "test-task", fs);

  /*let system_task =
        CurrentTask::create_system_task(locked, kernel, fs).expect("create system task");
    kernel.kthreads.init(system_task).expect("failed to initialize kthreads");*/

  return ktl::move(init_task.value());
}

template <typename CreateFsFn>
ktl::pair<fbl::RefPtr<Kernel>, starnix::testing::AutoReleasableTask>
create_kernel_task_and_unlocked_with_fs_and_selinux(
    CreateFsFn create_fs /*,security_server: Arc<SecurityServer>*/) {
  auto kernel = create_test_kernel();
  auto fs = create_test_fs_context(kernel, create_fs);
  auto init_task = create_test_init_task(kernel, fs);
  return ktl::pair(kernel, testing::AutoReleasableTask::From(ktl::move(init_task)));
}

/// Create a FileSystemHandle for use in testing.
///
/// Open "/boot" and returns an FsContext rooted in that directory.
FileSystemHandle create_bootfs(const fbl::RefPtr<Kernel>& kernel) {
  bootfs::testing::ZbiFile zbi;
  zbi.Write({kBootFsZbi, sizeof(kBootFsZbi) - 1});
  return BootFs::new_fs(kernel, HandleOwner(ktl::move(zbi).Finish()));
}

ktl::pair<fbl::RefPtr<Kernel>, starnix::testing::AutoReleasableTask>
create_kernel_task_and_unlocked_with_bootfs() {
  return create_kernel_task_and_unlocked_with_fs_and_selinux(create_bootfs);
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
