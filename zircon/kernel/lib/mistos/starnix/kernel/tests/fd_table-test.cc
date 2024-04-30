// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/starnix/kernel/vfs/fd_table.h"

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/fs/mistos/syslog.h>
#include <lib/mistos/starnix/kernel/fs/tmpfs.h>
#include <lib/mistos/starnix/kernel/task/module.h>
#include <lib/mistos/starnix/kernel/vfs/module.h>
#include <lib/mistos/starnix/testing/testing.h>

#include <zxtest/zxtest.h>

namespace {

using namespace starnix;
using namespace starnix_uapi;
using namespace starnix::testing;

fit::result<Errno, FdNumber> add(const CurrentTask& current_task, const FdTable& files,
                                 FileHandle file) {
  return files.add_with_flags(*current_task.task(), file, FdFlags::empty());
}

TEST(FdTable, test_fd_table_install) {
  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();
  auto files = FdTable::Create();
  auto file = SyslogFile::new_file(*current_task);

  auto fd0 = add(*current_task, files, file).value();
  ASSERT_EQ(0, fd0.raw());

  auto fd1 = add(*current_task, files, file).value();
  ASSERT_EQ(1, fd1.raw());

  ASSERT_EQ(file, files.get(fd0).value());
  ASSERT_EQ(file, files.get(fd1).value());

  ASSERT_EQ(errno(EBADF), files.get(FdNumber::from_raw(fd1.raw() + 1)).error_value());

  files.release();
}

TEST(FdTable, test_fd_table_pack_values) {
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
}

}  // namespace
