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

namespace testing {
namespace {

using starnix::FdFlags;
using starnix::FdFlagsEnum;
using starnix::FdNumber;

bool test_sys_dup2() {
  BEGIN_TEST;
  auto [kernel, current_task] =
      starnix::testing::create_kernel_task_and_unlocked_with_bootfs_current_zbi();
  auto fd = FdNumber::from_raw(42);
  auto result = sys_dup2(*current_task, fd, fd);
  ASSERT_TRUE(result.is_error());
  ASSERT_EQ(errno(EBADF).error_code(), result.error_value().error_code());

  auto file_handle = current_task->open_file("data/testfile.txt", OpenFlags(OpenFlagsEnum::RDONLY));
  ASSERT_TRUE(file_handle.is_ok(), "open_file");

  auto fd_or_error = (*current_task)->add_file(file_handle.value(), FdFlags::empty());
  ASSERT_TRUE(fd_or_error.is_ok(), "add");
  fd = fd_or_error.value();

  result = sys_dup2(*current_task, fd, fd);
  ASSERT_TRUE(result.is_ok());
  ASSERT_EQ(fd.raw(), result.value().raw());

  END_TEST;
}

bool test_sys_creat() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  auto path_addr =
      starnix::testing::map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  ktl::string_view path("newfile.txt");
  auto result = (*current_task).write_memory(path_addr, {(uint8_t*)path.data(), path.size()});
  ASSERT_TRUE(result.is_ok());
  auto fd_or_error =
      sys_creat(*current_task, UserCString::New(path_addr), starnix_uapi::FileMode());
  ASSERT_TRUE(fd_or_error.is_ok());
  auto file_handle = current_task->open_file(path, OpenFlags(OpenFlagsEnum::RDONLY));

  auto flag_or_error = (*current_task)->files_.get_fd_flags_allowing_opath(fd_or_error.value());
  ASSERT_TRUE(flag_or_error.is_ok());
  ASSERT_FALSE(flag_or_error->contains(FdFlagsEnum::CLOEXEC));

  END_TEST;
}

#if 0
bool test_time() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();
  auto time1_or_error = sys_time(*current_task, mtl::DefaultConstruct<UserRef<__kernel_time_t>>());
  ASSERT_TRUE(time1_or_error.is_ok());
  auto time1 = time1_or_error.value();
  ASSERT_GT(time1, 0);

  auto address =
      map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), sizeof(__kernel_time_t));

  zx::MonotonicDuration::from_seconds(2).sleep();

  auto time2_or_error = sys_time(*current_task, UserRef<__kernel_time_t>::New(address));
  ASSERT_TRUE(time2_or_error.is_ok());
  auto time2 = time2_or_error.value();
  ASSERT_GE(time2, time1 + 2);
  ASSERT_LT(time2, time1 + 10);

  auto time3_or_error = (*current_task).read_object<__kernel_time_t>(address);
  ASSERT_TRUE(time3_or_error.is_ok());
  ASSERT_EQ(time2, time3_or_error.value());

  END_TEST;
}
#endif

}  // namespace
}  // namespace testing

UNITTEST_START_TESTCASE(starnix_arch_syscalls)
UNITTEST("test sys dup2", testing::test_sys_dup2)
UNITTEST("test sys creat", testing::test_sys_creat)
// UNITTEST("test time", testing::test_time)
UNITTEST_END_TESTCASE(starnix_arch_syscalls, "starnix_arch_syscalls", "Tests for Tasks Syscalls")
