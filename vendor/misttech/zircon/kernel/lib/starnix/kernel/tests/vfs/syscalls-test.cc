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
#include <lib/unittest/unittest.h>

#include <fbl/ref_ptr.h>
#include <ktl/string_view.h>

using namespace starnix::testing;

namespace unit_testing {

bool test_sys_dup() {
  BEGIN_TEST;
  END_TEST;
}

bool test_sys_dup3() {
  BEGIN_TEST;
  END_TEST;
}

bool test_sys_open_cloexec() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked_with_pkgfs();

  auto path_addr = map_memory(*current_task, UserAddress(), PAGE_SIZE);
  ktl::string_view path("data/testfile.txt");
  auto result = (*current_task).write_memory(path_addr, {(uint8_t*)path.data(), path.size()});
  ASSERT_TRUE(result.is_ok());

  auto fd_or_error = sys_openat(*current_task, starnix::FdNumber::_AT_FDCWD, UserCString(path_addr),
                                O_RDONLY | O_CLOEXEC, starnix_uapi::FileMode());
  ASSERT_TRUE(fd_or_error.is_ok(), "failed to sys_openat");

  auto flag_or_error = current_task->files.get_fd_flags(fd_or_error.value());
  ASSERT_TRUE(flag_or_error.is_ok());
  ASSERT_TRUE(flag_or_error.value().contains(starnix::FdFlagsEnum::CLOEXEC));

  END_TEST;
}

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_vfs_syscalls)
// UNITTEST("test sys dup", testing::test_sys_dup)
// UNITTEST("test sys dup3", testing::test_sys_dup3)
UNITTEST("test sys open cloexec", unit_testing::test_sys_open_cloexec)
UNITTEST_END_TESTCASE(starnix_vfs_syscalls, "starnix_vfs_syscalls", "Tests for VFS Syscalls")
