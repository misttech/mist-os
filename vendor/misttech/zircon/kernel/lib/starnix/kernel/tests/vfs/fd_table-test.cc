// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/fd_table.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/fs/mistos/syslog.h>
#include <lib/mistos/starnix/kernel/fs/tmpfs.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/unittest/unittest.h>

#include <utility>

#include <fbl/ref_ptr.h>

namespace unit_testing {
namespace {

using starnix::CurrentTask;
using starnix::FdFlags;
using starnix::FdFlagsEnum;
using starnix::FdNumber;
using starnix::FdTable;
using starnix::FileHandle;
using starnix::FileObject;
using starnix::SyslogFile;

fit::result<Errno, FdNumber> add(const CurrentTask& current_task, const FdTable& files,
                                 FileHandle file) {
  return files.add_with_flags(*current_task.task(), ktl::move(file), FdFlags::empty());
}

bool test_fd_table_install() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();
  auto files = FdTable::Create();
  auto file = SyslogFile::new_file(*current_task);

  auto fd0 = add(*current_task, files, fbl::RefPtr<FileObject>(file.get())).value();
  ASSERT_EQ(0, fd0.raw());

  auto fd1 = add(*current_task, files, fbl::RefPtr<FileObject>(file.get())).value();
  ASSERT_EQ(1, fd1.raw());

  ASSERT_EQ(file.get(), files.get(fd0).value().get());
  ASSERT_EQ(file.get(), files.get(fd1).value().get());

  ASSERT_EQ(errno(EBADF).error_code(),
            files.get(FdNumber::from_raw(fd1.raw() + 1)).error_value().error_code());

  files.release();

  END_TEST;
}

bool test_fd_table_fork() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();
  auto files = FdTable::Create();
  auto file = SyslogFile::new_file(*current_task);

  auto fd0 = add(*current_task, files, fbl::RefPtr<FileObject>(file.get())).value();
  auto fd1 = add(*current_task, files, file).value();
  auto fd2 = FdNumber::from_raw(2);

  auto forked = files.fork();

  ASSERT_TRUE(files.get(fd0).value() == forked.get(fd0).value());
  ASSERT_TRUE(files.get(fd1).value() == forked.get(fd1).value());

  ASSERT_TRUE(files.get(fd2).is_error());
  ASSERT_TRUE(forked.get(fd2).is_error());

  ASSERT_TRUE(files.set_fd_flags(fd0, FdFlags(FdFlagsEnum::CLOEXEC)).is_ok());
  ASSERT_EQ(FdFlags(FdFlagsEnum::CLOEXEC).bits(), files.get_fd_flags(fd0).value().bits());
  ASSERT_NE(FdFlags(FdFlagsEnum::CLOEXEC).bits(), forked.get_fd_flags(fd0).value().bits());

  forked.release();
  files.release();

  END_TEST;
}

bool test_fd_table_exec() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();
  auto files = FdTable::Create();
  auto file = SyslogFile::new_file(*current_task);

  auto fd0 = add(*current_task, files, fbl::RefPtr<FileObject>(file.get())).value();
  auto fd1 = add(*current_task, files, file).value();

  ASSERT_TRUE(files.set_fd_flags(fd0, FdFlags(FdFlagsEnum::CLOEXEC)).is_ok());

  ASSERT_TRUE(files.get(fd0).is_ok());
  ASSERT_TRUE(files.get(fd1).is_ok());

  files.exec();

  ASSERT_TRUE(files.get(fd0).is_error());
  ASSERT_TRUE(files.get(fd1).is_ok());

  files.release();

  END_TEST;
}

bool test_fd_table_pack_values() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();
  auto files = FdTable::Create();
  auto file = SyslogFile::new_file(*current_task);

  // Add two FDs.
  auto fd0 = add(*current_task, files, file).value();
  auto fd1 = add(*current_task, files, file).value();
  ASSERT_EQ(0, fd0.raw());
  ASSERT_EQ(1, fd1.raw());

  // Close FD 0
  ASSERT_TRUE(files.close(fd0).is_ok());
  ASSERT_TRUE(files.close(fd0).is_error());
  // Now it's gone.
  ASSERT_TRUE(files.get(fd0).is_error());

  // The next FD we insert fills in the hole we created.
  auto another_fd = add(*current_task, files, file).value();
  ASSERT_EQ(0, another_fd.raw());

  files.release();

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_fd_table)
UNITTEST("test fd table install", unit_testing::test_fd_table_install)
UNITTEST("test fd table fork", unit_testing::test_fd_table_fork)
UNITTEST("test fd table exec", unit_testing::test_fd_table_exec)
UNITTEST("test fd table pack values", unit_testing::test_fd_table_pack_values)
UNITTEST_END_TESTCASE(starnix_fd_table, "starnix_fd_table", "Tests for FdTable")
