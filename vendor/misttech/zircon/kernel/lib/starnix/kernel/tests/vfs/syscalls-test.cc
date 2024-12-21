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
using starnix::RenameFlags;
using starnix::RenameFlagsEnum;

bool test_sys_lseek() {
  BEGIN_TEST;

  auto [kernel, current_task] =
      starnix::testing::create_kernel_task_and_unlocked_with_bootfs_current_zbi();
  auto fd = FdNumber::from_raw(10);
  auto file_handle = current_task->open_file("data/testfile.txt", OpenFlags(OpenFlagsEnum::RDONLY));
  ASSERT_TRUE(file_handle.is_ok());
  auto file_size = file_handle.value()->node()->stat(*current_task).value().st_size;
  auto insert_result = (*current_task)->files_.insert(**current_task, fd, file_handle.value());
  ASSERT_TRUE(insert_result.is_ok());

  auto seek_cur_0 = sys_lseek(*current_task, fd, 0, SEEK_CUR);
  ASSERT_TRUE(seek_cur_0.is_ok());
  ASSERT_EQ(0, seek_cur_0.value());

  auto seek_cur_1 = sys_lseek(*current_task, fd, 1, SEEK_CUR);
  ASSERT_TRUE(seek_cur_1.is_ok());
  ASSERT_EQ(1, seek_cur_1.value());

  auto seek_set_3 = sys_lseek(*current_task, fd, 3, SEEK_SET);
  ASSERT_TRUE(seek_set_3.is_ok());
  ASSERT_EQ(3, seek_set_3.value());

  auto seek_cur_neg3 = sys_lseek(*current_task, fd, -3, SEEK_CUR);
  ASSERT_TRUE(seek_cur_neg3.is_ok());
  ASSERT_EQ(0, seek_cur_neg3.value());

  auto seek_end_0 = sys_lseek(*current_task, fd, 0, SEEK_END);
  ASSERT_TRUE(seek_end_0.is_ok());
  ASSERT_EQ(file_size, seek_end_0.value());

  auto seek_set_neg5 = sys_lseek(*current_task, fd, -5, SEEK_SET);
  ASSERT_TRUE(seek_set_neg5.is_error());
  ASSERT_EQ(seek_set_neg5.error_value().error_code(), errno(EINVAL).error_code());

  // Make sure failed call didn't change offset
  auto seek_cur_check = sys_lseek(*current_task, fd, 0, SEEK_CUR);
  ASSERT_TRUE(seek_cur_check.is_ok());
  ASSERT_EQ(seek_cur_check.value(), file_size);

  // Prepare for overflow
  auto seek_set_3_again = sys_lseek(*current_task, fd, 3, SEEK_SET);
  ASSERT_TRUE(seek_set_3_again.is_ok());
  ASSERT_EQ(seek_set_3_again.value(), 3);

  // Check for overflow
  auto seek_cur_max = sys_lseek(*current_task, fd, INT64_MAX, SEEK_CUR);
  ASSERT_TRUE(seek_cur_max.is_error());
  ASSERT_EQ(seek_cur_max.error_value().error_code(), errno(EINVAL).error_code());

  END_TEST;
}

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

  auto flag_or_error = (*current_task)->files_.get_fd_flags_allowing_opath(fd_or_error.value());
  ASSERT_TRUE(flag_or_error.is_ok());
  ASSERT_TRUE(flag_or_error.value().contains(starnix::FdFlagsEnum::CLOEXEC));

  END_TEST;
}

bool test_fstat_tmp_file() {
  BEGIN_TEST;

  auto [kernel, current_task] =
      starnix::testing::create_kernel_task_and_unlocked_with_bootfs_current_zbi();

  // Create the file that will be used to stat
  auto file_handle = current_task->open_file("data/testfile.txt", OpenFlags(OpenFlagsEnum::RDONLY));
  ASSERT_TRUE(file_handle.is_ok());

  // Write path to user memory
  auto path_addr =
      starnix::testing::map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  ktl::string_view path("data/testfile.txt");
  auto write_result = (*current_task).write_memory(path_addr, {(uint8_t*)path.data(), path.size()});
  ASSERT_TRUE(write_result.is_ok());

  auto user_stat = starnix_uapi::UserRef<struct ::statfs>::New(path_addr + path.size());
  auto write_stat_result = (*current_task).write_object(user_stat, default_statfs(0));
  ASSERT_TRUE(write_stat_result.is_ok());

  auto statfs_result = sys_statfs(*current_task, UserCString::New(path_addr), user_stat);
  ASSERT_TRUE(statfs_result.is_ok());

  auto returned_stat = (*current_task).read_object(user_stat);
  ASSERT_TRUE(returned_stat.is_ok());

  // auto expected_stat =
  //     default_statfs(starnix::util::from_be32(*reinterpret_cast<const uint32_t*>("f.io")));
  // ASSERT_EQ(memcmp(&returned_stat.value(), &expected_stat, sizeof(struct ::statfs)), 0);

  END_TEST;
}

bool test_unlinkat_dir() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();

  // Create the dir that we will attempt to unlink later
  auto no_slash_path_addr =
      starnix::testing::map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  ktl::string_view no_slash_path("testdir");
  auto write_result =
      (*current_task)
          .write_memory(no_slash_path_addr, {(uint8_t*)no_slash_path.data(), no_slash_path.size()});
  ASSERT_TRUE(write_result.is_ok());
  auto no_slash_user_path = UserCString::New(no_slash_path_addr);

  auto mkdir_result =
      sys_mkdirat(*current_task, FdNumber::AT_FDCWD_, UserCString::New(no_slash_path_addr),
                  FileMode::ALLOW_ALL.with_type(FileMode::IFDIR));
  ASSERT_TRUE(mkdir_result.is_ok());

  auto slash_path_addr =
      starnix::testing::map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  ktl::string_view slash_path("testdir/");
  write_result =
      (*current_task)
          .write_memory(slash_path_addr, {(uint8_t*)slash_path.data(), slash_path.size()});
  ASSERT_TRUE(write_result.is_ok());
  auto slash_user_path = UserCString::New(slash_path_addr);

  // Try to remove directory without AT_REMOVEDIR
  // Should fail with EISDIR regardless of trailing slash
  auto unlink_result = sys_unlinkat(*current_task, FdNumber::AT_FDCWD_, slash_user_path, 0);
  ASSERT_TRUE(unlink_result.is_error());
  ASSERT_EQ(errno(EISDIR).error_code(), unlink_result.error_value().error_code());

  unlink_result = sys_unlinkat(*current_task, FdNumber::AT_FDCWD_, no_slash_user_path, 0);
  ASSERT_TRUE(unlink_result.is_error());
  ASSERT_EQ(errno(EISDIR).error_code(), unlink_result.error_value().error_code());

  // Success with AT_REMOVEDIR
  unlink_result = sys_unlinkat(*current_task, FdNumber::AT_FDCWD_, slash_user_path, AT_REMOVEDIR);
  ASSERT_TRUE(unlink_result.is_ok());

  END_TEST;
}

bool test_rename_noreplace() {
  BEGIN_TEST;

  auto [kernel, current_task] =
      starnix::testing::create_kernel_task_and_unlocked_with_bootfs_current_zbi();

  // Create the file that will be renamed
  ktl::string_view old_path("data/testfile.txt");
  auto old_file_handle = current_task->open_file(old_path, OpenFlags(OpenFlagsEnum::RDONLY));
  ASSERT_TRUE(old_file_handle.is_ok());

  // Write old path to user memory
  auto old_path_addr =
      starnix::testing::map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  auto write_result =
      (*current_task)->write_memory(old_path_addr, {(uint8_t*)old_path.data(), old_path.size()});
  ASSERT_TRUE(write_result.is_ok());

  // Create second file that we'll attempt to rename to
  ktl::string_view new_path("data/testfile2.txt");
  auto new_file_handle = current_task->open_file(new_path, OpenFlags(OpenFlagsEnum::RDONLY));
  ASSERT_TRUE(new_file_handle.is_ok());

  // Write new path to user memory
  auto new_path_addr =
      starnix::testing::map_memory(*current_task, mtl::DefaultConstruct<UserAddress>(), PAGE_SIZE);
  write_result =
      (*current_task)->write_memory(new_path_addr, {(uint8_t*)new_path.data(), new_path.size()});
  ASSERT_TRUE(write_result.is_ok());

  // Try to rename first file to second file's name with RENAME_NOREPLACE flag
  // Should fail with EEXIST
  auto rename_result = sys_renameat2(
      *current_task, FdNumber::AT_FDCWD_, UserCString::New(old_path_addr), FdNumber::AT_FDCWD_,
      UserCString::New(new_path_addr), RenameFlags(RenameFlagsEnum::NOREPLACE).bits());
  ASSERT_TRUE(rename_result.is_error());
  ASSERT_EQ(rename_result.error_value().error_code(), errno(EEXIST).error_code());

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_vfs_syscalls)
UNITTEST("test sys lseek", unit_testing::test_sys_lseek)
UNITTEST("test sys dup", unit_testing::test_sys_dup)
UNITTEST("test sys dup3", unit_testing::test_sys_dup3)
UNITTEST("test sys open cloexec", unit_testing::test_sys_open_cloexec)
UNITTEST("test sys statfs", unit_testing::test_fstat_tmp_file)
UNITTEST("test unlinkat dir", unit_testing::test_unlinkat_dir)
UNITTEST("test rename noreplace", unit_testing::test_rename_noreplace)
UNITTEST_END_TESTCASE(starnix_vfs_syscalls, "starnix_vfs_syscalls", "Tests for VFS Syscalls")
