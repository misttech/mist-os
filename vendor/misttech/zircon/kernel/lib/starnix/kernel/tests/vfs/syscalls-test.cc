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

namespace unit_testing {
namespace {

using starnix::FdFlags;
using starnix::FdFlagsEnum;
using starnix::FdNumber;

bool test_sys_dup() {
  BEGIN_TEST;

  auto [kernel, current_task] =
      starnix::testing::create_kernel_task_and_unlocked_with_bootfs_current_zbi();
  auto file_handle = current_task->open_file("data/testfile.txt", OpenFlags(OpenFlagsEnum::RDONLY));
  ASSERT_TRUE(file_handle.is_ok());
  auto oldfd = (*current_task)->add_file(file_handle.value(), FdFlags::empty());
  ASSERT_TRUE(oldfd.is_ok());
  auto newfd = sys_dup(*current_task, oldfd.value());
  ASSERT_TRUE(newfd.is_ok());

  ASSERT_NE(newfd->raw(), oldfd->raw());

  auto& files = (*current_task)->files_;
  auto old_file = files.get(oldfd.value());
  ASSERT_TRUE(old_file.is_ok());
  auto new_file = files.get(newfd.value());
  ASSERT_TRUE(new_file.is_ok());
  ASSERT_TRUE(old_file.value() == new_file.value());

  auto bad_fd = sys_dup(*current_task, FdNumber::from_raw(3));
  ASSERT_TRUE(bad_fd.is_error());
  ASSERT_EQ(errno(EBADF).error_code(), bad_fd.error_value().error_code());

  END_TEST;
}

bool test_sys_dup3() {
  BEGIN_TEST;

  auto [kernel, current_task] =
      starnix::testing::create_kernel_task_and_unlocked_with_bootfs_current_zbi();
  auto file_handle = current_task->open_file("data/testfile.txt", OpenFlags(OpenFlagsEnum::RDONLY));
  ASSERT_TRUE(file_handle.is_ok());
  auto oldfd = (*current_task)->add_file(file_handle.value(), FdFlags::empty());
  ASSERT_TRUE(oldfd.is_ok());
  auto newfd = FdNumber::from_raw(2);
  auto dup3_result = sys_dup3(*current_task, oldfd.value(), newfd, O_CLOEXEC);
  ASSERT_TRUE(dup3_result.is_ok());

  ASSERT_NE(newfd.raw(), oldfd->raw());

  auto& files = (*current_task)->files_;
  auto old_file = files.get(oldfd.value());
  ASSERT_TRUE(old_file.is_ok());
  auto new_file = files.get(newfd);
  ASSERT_TRUE(new_file.is_ok());
  ASSERT_TRUE(old_file.value() == new_file.value());

  auto old_flags = files.get_fd_flags_allowing_opath(oldfd.value());
  ASSERT_TRUE(old_flags.is_ok());
  ASSERT_TRUE(old_flags.value().is_empty());

  auto new_flags = files.get_fd_flags_allowing_opath(newfd);
  ASSERT_TRUE(new_flags.is_ok());
  ASSERT_TRUE(new_flags.value().contains(FdFlagsEnum::CLOEXEC));

  auto same_fd_result = sys_dup3(*current_task, oldfd.value(), oldfd.value(), O_CLOEXEC);
  ASSERT_TRUE(same_fd_result.is_error());
  ASSERT_EQ(errno(EINVAL).error_code(), same_fd_result.error_value().error_code());

  auto invalid_flags = 1234;
  auto invalid_flags_result = sys_dup3(*current_task, oldfd.value(), newfd, invalid_flags);
  ASSERT_TRUE(invalid_flags_result.is_error());
  ASSERT_EQ(errno(EINVAL).error_code(), invalid_flags_result.error_value().error_code());

  auto second_file_handle =
      current_task->open_file("data/testfile.txt", OpenFlags(OpenFlagsEnum::RDONLY));
  ASSERT_TRUE(second_file_handle.is_ok());
  auto different_file_fd = (*current_task)->add_file(second_file_handle.value(), FdFlags::empty());
  ASSERT_TRUE(different_file_fd.is_ok());

  auto old_file2 = files.get(oldfd.value());
  ASSERT_TRUE(old_file2.is_ok());
  auto diff_file = files.get(different_file_fd.value());
  ASSERT_TRUE(diff_file.is_ok());
  ASSERT_FALSE(old_file2.value() == diff_file.value());

  auto dup3_result2 = sys_dup3(*current_task, oldfd.value(), different_file_fd.value(), O_CLOEXEC);
  ASSERT_TRUE(dup3_result2.is_ok());

  auto old_file3 = files.get(oldfd.value());
  ASSERT_TRUE(old_file3.is_ok());
  auto diff_file2 = files.get(different_file_fd.value());
  ASSERT_TRUE(diff_file2.is_ok());
  ASSERT_TRUE(old_file3.value() == diff_file2.value());

  END_TEST;
}

bool test_sys_open_cloexec() {
  BEGIN_TEST;

  auto [kernel, current_task] =
      starnix::testing::create_kernel_task_and_unlocked_with_bootfs_current_zbi();

  auto path_addr =
      starnix::testing::map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  ktl::string_view path("data/testfile.txt");
  auto result = (*current_task).write_memory(path_addr, {(uint8_t*)path.data(), path.size()});
  ASSERT_TRUE(result.is_ok());

  auto fd_or_error =
      sys_openat(*current_task, starnix::FdNumber::AT_FDCWD_, UserCString::New(path_addr),
                 O_RDONLY | O_CLOEXEC, starnix_uapi::FileMode());
  ASSERT_TRUE(fd_or_error.is_ok(), "failed to sys_openat");

  auto flag_or_error = (*current_task)->files_.get_fd_flags(fd_or_error.value());
  ASSERT_TRUE(flag_or_error.is_ok());
  ASSERT_TRUE(flag_or_error.value().contains(starnix::FdFlagsEnum::CLOEXEC));

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_vfs_syscalls)
UNITTEST("test sys dup", unit_testing::test_sys_dup)
UNITTEST("test sys dup3", unit_testing::test_sys_dup3)
UNITTEST("test sys open cloexec", unit_testing::test_sys_open_cloexec)
UNITTEST_END_TESTCASE(starnix_vfs_syscalls, "starnix_vfs_syscalls", "Tests for VFS Syscalls")
