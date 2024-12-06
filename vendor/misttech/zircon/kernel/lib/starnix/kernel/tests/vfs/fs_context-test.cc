// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/result.h>
#include <lib/mistos/starnix/kernel/fs/tmpfs.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/fs_context.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/mistos/util/testing/unittest.h>
#include <lib/unittest/unittest.h>

#include <fbl/ref_ptr.h>

namespace unit_testing {

namespace {

using starnix::FsContext;
using starnix::Namespace;
using starnix::TmpFs;

bool test_umask() {
  BEGIN_TEST;
  auto [kernel, _task] = starnix::testing::create_kernel_and_task();

  auto fs = FsContext::New(Namespace::New(TmpFs::new_fs(kernel)));

  ASSERT_TRUE(FileMode::from_bits(022) == fs->set_umask(FileMode::from_bits(03020)));
  ASSERT_TRUE(FileMode::from_bits(0646) == fs->apply_umask(FileMode::from_bits(0666)));
  ASSERT_TRUE(FileMode::from_bits(03646) == fs->apply_umask(FileMode::from_bits(03666)));
  ASSERT_TRUE(FileMode::from_bits(020) == fs->set_umask(FileMode::from_bits(011)));

  END_TEST;
}

bool test_chdir() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked_with_bootfs();

  ASSERT_STREQ("/", (*current_task)->fs()->cwd().path_escaping_chroot());
  END_TEST;
}

}  // namespace

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_fs_context)
UNITTEST("test umask", unit_testing::test_umask)
UNITTEST("test chdir", unit_testing::test_chdir)
UNITTEST_END_TESTCASE(starnix_fs_context, "starnix_fs_context", "Tests for FsContext")
