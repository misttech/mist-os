// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/testing/testing.h"

#include <lib/handoff/handoff.h>
#include <lib/mistos/starnix/kernel/fs/mistos/bootfs.h>
#include <lib/mistos/starnix/kernel/fs/tmpfs.h>
#include <lib/mistos/starnix/kernel/mm/flags.h>
#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/starnix/kernel/mm/syscalls.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_system.h>
#include <lib/mistos/starnix/kernel/vfs/fs_context.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/namespace.h>
#include <lib/mistos/util/error_propagation.h>
#include <lib/starnix/bootfs/tests/data/bootfs.zbi.h>
#include <lib/starnix/bootfs/tests/zbi_file.h>
#include <zircon/assert.h>

#include <fbl/ref_ptr.h>

namespace starnix::testing {

fbl::RefPtr<Kernel> create_test_kernel(/*security_server: Arc<SecurityServer>*/) {
  return Kernel::New("").value();
}

namespace {

template <typename CreateFsFn>
fbl::RefPtr<FsContext> create_test_fs_context(const fbl::RefPtr<Kernel>& kernel,
                                              CreateFsFn&& create_fs) {
  return FsContext::New(Namespace::New(create_fs(kernel)));
}

}  // namespace

TaskBuilder create_test_init_task(fbl::RefPtr<Kernel> kernel, fbl::RefPtr<FsContext> fs) {
  auto init_pid = kernel->pids_.Write()->allocate_pid();
  ASSERT(init_pid == 1);
  fbl::Array<ktl::pair<starnix_uapi::Resource, uint64_t>> rlimits;
  auto init_task =
      CurrentTask::create_init_process(kernel, init_pid, "test-task", fs, ktl::move(rlimits));

  ZX_ASSERT_MSG(init_task.is_ok(), "failed to create first task");

  init_task->mm()->initialize_mmap_layout_for_test();

  auto system_task = CurrentTask::create_system_task(kernel, fs);
  ZX_ASSERT_MSG(system_task.is_ok(), "create system task");
  ZX_ASSERT_MSG(kernel->kthreads_.Init(ktl::move(system_task.value())).is_ok(),
                "failed to initialize kthreads");

  // let system_task = kernel.kthreads.system_task();
  // kernel.hrtimer_manager.init(&system_task).expect("init hrtimer manager worker thread");

  // Take the lock on thread group and task in the correct order to ensure any wrong ordering
  // will trigger the tracing-mutex at the right call site.
  {
    auto _l1 = init_task->thread_group_->Read();
    auto _l2 = init_task->mutable_state_.Read();
  }

  return ktl::move(init_task.value());
}

template <typename CreateFsFn>
ktl::pair<fbl::RefPtr<Kernel>, starnix::testing::AutoReleasableTask>
create_kernel_task_and_unlocked_with_fs_and_selinux(
    CreateFsFn&& create_fs /*,security_server: Arc<SecurityServer>*/) {
  auto kernel = create_test_kernel();
  auto fs = create_fs(kernel);
  auto fs_context =
      create_test_fs_context(kernel, [&fs](const fbl::RefPtr<Kernel>&) { return fs; });
  auto init_task = create_test_init_task(kernel, fs_context);
  // security::file_system_resolve_security(&init_task, &fs)
  //       .expect("Failed to resolve root filesystem labeling");
  return ktl::pair(kernel, testing::AutoReleasableTask::From(ktl::move(init_task)));
}

/// Create a FileSystemHandle for use in testing.
///
/// Open "/boot" and returns an FsContext rooted in that directory.
FileSystemHandle create_bootfs(const fbl::RefPtr<Kernel>& kernel) {
  bootfs::testing::ZbiFile zbi("BootFsZbi");
  zbi.Write({kBootFsZbi, sizeof(kBootFsZbi) - 1});
  fbl::AllocChecker ac;
  zx::vmo vmo(fbl::MakeRefCountedChecked<zx::Value>(&ac, ktl::move(zbi).Finish()));
  ZX_ASSERT(ac.check());
  return BootFs::new_fs(kernel, vmo.borrow());
}

FileSystemHandle create_bootfs_current_zbi(const fbl::RefPtr<Kernel>& kernel) {
  return BootFs::new_fs(kernel, GetZbi());
}

ktl::pair<fbl::RefPtr<Kernel>, starnix::testing::AutoReleasableTask>
create_kernel_task_and_unlocked_with_bootfs() {
  return create_kernel_task_and_unlocked_with_fs_and_selinux(create_bootfs);
}

ktl::pair<fbl::RefPtr<Kernel>, starnix::testing::AutoReleasableTask>
create_kernel_task_and_unlocked_with_bootfs_current_zbi() {
  return create_kernel_task_and_unlocked_with_fs_and_selinux(create_bootfs_current_zbi);
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
  auto task = CurrentTask::create_init_child_process(kernel, task_name);
  ZX_ASSERT_MSG(task.is_ok(), "failed to create second task");
  task->mm()->initialize_mmap_layout_for_test();

  // Take the lock on thread group and task in the correct order to ensure any wrong ordering
  // will trigger the tracing-mutex at the right call site.
  {
    auto _l1 = task->thread_group_->Read();
    auto _l2 = task->Read();
  }

  return ktl::move(testing::AutoReleasableTask::From(ktl::move(task.value())));
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

FileSystemHandle create_fs(fbl::RefPtr<starnix::Kernel>& kernel, FsNodeOps* ops) {
  fbl::AllocChecker ac;
  auto ptr = new (&ac) TestFs();
  ZX_ASSERT(ac.check());

  auto test_fs =
      FileSystem::New(kernel, {.type = CacheModeType::Uncached}, ptr, FileSystemOptions());
  ZX_ASSERT_MSG(test_fs, "testfs constructed with valid options");
  auto bus_dir_node = FsNode::new_root(ops);
  test_fs->set_root_node(bus_dir_node);
  return test_fs;
}

}  // namespace starnix::testing
