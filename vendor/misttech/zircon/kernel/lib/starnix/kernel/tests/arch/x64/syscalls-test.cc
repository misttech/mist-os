// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/arch/x64/syscalls.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/kernel/vfs/file_object.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/mistos/starnix_uapi/open_flags.h>
#include <lib/unittest/unittest.h>

#include <fbl/ref_ptr.h>
#include <ktl/string_view.h>

using namespace starnix::testing;
using namespace starnix_uapi;

namespace testing {

bool test_sys_creat() {
  BEGIN_TEST;
  auto [kernel, current_task] = create_kernel_and_task();
  auto path_addr = map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  ktl::string_view path("newfile.txt");
  auto result = (*current_task).write_memory(path_addr, {(uint8_t*)path.data(), path.size()});
  ASSERT_TRUE(result.is_ok());
  auto fd_or_error =
      sys_creat(*current_task, UserCString::New(path_addr), starnix_uapi::FileMode());
  ASSERT_TRUE(fd_or_error.is_ok());
  auto file_handle = (*current_task).open_file(path, OpenFlags(OpenFlagsEnum::RDONLY));

  auto flag_or_error = current_task->files().get_fd_flags(fd_or_error.value());
  ASSERT_TRUE(flag_or_error.is_ok());

  END_TEST;
}

}  // namespace testing

UNITTEST_START_TESTCASE(starnix_arch_syscalls)
UNITTEST("test sys creat", testing::test_sys_creat)
UNITTEST_END_TESTCASE(starnix_arch_syscalls, "starnix_arch_syscalls", "Tests for Tasks Syscalls")
