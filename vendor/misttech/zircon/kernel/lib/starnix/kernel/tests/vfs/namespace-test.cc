// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/fs/tmpfs.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/lookup_context.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {

using starnix::LookupContext;
using starnix::Namespace;
using starnix::TmpFs;

namespace {
bool test_namespace() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  auto root_fs = TmpFs::new_fs(kernel);
  auto root_node = root_fs->root();

  ASSERT_TRUE(root_node->create_dir(*current_task, "dev").is_ok(), "failed to mkdir dev");

  auto dev_fs = TmpFs::new_fs(kernel);
  auto dev_root_node = dev_fs->root();
  ASSERT_TRUE(dev_root_node->create_dir(*current_task, "pts").is_ok(), "failed to mkdir pts");

  auto ns = Namespace::New(root_fs);
  auto context = LookupContext::Default();

  auto dev = ns->root().lookup_child(*current_task, context, "dev");
  ASSERT_TRUE(dev.is_ok(), "failed to lookup dev");

  // auto dev = ASSERT_OK();
  // dev->mount({.type = WhatToMountEnum::Fs, .what = dev_fs}, MountFlags::Empty()));

  /*context = LookupContext::Default();
  dev = ASSERT_OK(ns->root()->LookupChild(locked.get(), current_task.get(), &context, "dev"));

  context = LookupContext::Default();
  auto pts = ASSERT_OK(dev->LookupChild(locked.get(), current_task.get(), &context, "pts"));

  auto pts_parent = ASSERT_OK(pts->parent());
  ASSERT_TRUE(pts_parent->entry() == dev->entry());

  auto dev_parent = ASSERT_OK(dev->parent());
  ASSERT_TRUE(dev_parent->entry() == ns->root()->entry());
  */
  END_TEST;
}
}  // namespace

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_vfs_namespace)
UNITTEST("test namespace", unit_testing::test_namespace)
UNITTEST_END_TESTCASE(starnix_vfs_namespace, "starnix_vfs_namespace", "Tests for VFS Namespace")
