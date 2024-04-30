// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/loader.h>
#include <lib/mistos/starnix/kernel/mm/memory_manager.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/testing/testing.h>

#include <zxtest/zxtest.h>

namespace {

fit::result<Errno> exec_hello_starnix(starnix::CurrentTask& current_task) {
  auto argv = std::vector<fbl::String>{"bin/hello_starnix"};
  auto executable = current_task.open_file_bootfs(argv[0] /*, OpenFlags::RDONLY*/);
  if (executable.is_error())
    return executable.take_error();
  return current_task.exec(executable.value(), argv[0], argv, std::vector<fbl::String>());
}

TEST(Loader, test_load_hello_starnix) {
  fbl::RefPtr<starnix::Kernel> kernel;
  starnix::testing::AutoReleasableTask current_task;

  // auto result = starnix::testing::create_kernel_task_and_unlocked_with_pkgfs();
  auto result = starnix::testing::create_kernel_and_task();
  std::tie(kernel, current_task) = result;

  auto errno = exec_hello_starnix(*current_task);
  ASSERT_FALSE(errno.is_error(), "errno %u", errno.error_value().error_code());
  ASSERT_GT(current_task->mm()->get_mapping_count(), 0);
}

}  // namespace
