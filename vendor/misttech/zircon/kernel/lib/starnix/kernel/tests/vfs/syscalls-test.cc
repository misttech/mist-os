// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/fd_table.h>
#include <lib/mistos/starnix/kernel/vfs/syscalls.h>
#include <lib/mistos/starnix/testing/testing.h>

#include <fbl/ref_ptr.h>
#include <zxtest/zxtest.h>

using namespace starnix::testing;
using namespace starnix;

namespace {

TEST(Vfs, DISABLED_test_sys_dup) {}

TEST(Vfs, DISABLED_test_sys_dup3) {}

TEST(Vfs, test_sys_open_cloexec) {
  auto [kernel, current_task] = create_kernel_task_and_unlocked_with_pkgfs();

  auto path_addr = map_memory(*current_task, UserAddress(), PAGE_SIZE);
  fbl::String path("data/testfile.txt");
  auto result = (*current_task).write_memory(path_addr, {(uint8_t*)path.data(), path.size()});
  ASSERT_TRUE(result.is_ok());

  auto fd_or_error = sys_openat(*current_task, FdNumber::_AT_FDCWD, UserCString(path_addr),
                                O_RDONLY | O_CLOEXEC, starnix_uapi::FileMode());
  ASSERT_TRUE(fd_or_error.is_ok(), "failed to sys_openat, error %d",
              fd_or_error.error_value().error_code());

  auto flag_or_error = current_task->files.get_fd_flags(fd_or_error.value());
  ASSERT_TRUE(flag_or_error.is_ok());
  ASSERT_TRUE(flag_or_error.value().contains(FdFlagsEnum::CLOEXEC));
}

}  // namespace
