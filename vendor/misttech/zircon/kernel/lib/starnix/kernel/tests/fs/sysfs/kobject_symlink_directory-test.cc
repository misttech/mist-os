// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/device/registry.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/vfs/lookup_context.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {

using starnix::FsNodeOps;

namespace {

fit::result<Errno, starnix::NamespaceNode> lookup_node(const starnix::CurrentTask& task,
                                                       const starnix::FileSystemHandle& fs,
                                                       const starnix::FsStr& name) {
  auto root = starnix::NamespaceNode::new_anonymous(fs->root());
  auto context = starnix::LookupContext::New(starnix::SymlinkMode::NoFollow);
  return task.lookup_path(context, root, name);
}

bool kobject_symlink_directory_contains_device_links() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();

  auto root_kobject = starnix::KObject::new_root("");
  root_kobject->get_or_create_child<FsNodeOps>("0", starnix::KObjectDirectory::New);
  root_kobject->get_or_create_child<FsNodeOps>("0", starnix::KObjectDirectory::New);
  auto test_fs = starnix::testing::create_fs(
      kernel, starnix::KObjectSymlinkDirectory::New(root_kobject->weak_factory_.GetWeakPtr()));

  auto device_entry = lookup_node(*current_task, test_fs, "0");
  ASSERT_TRUE(device_entry.is_ok(), "device 0 directory");
  ASSERT_TRUE(device_entry->entry_->node_->is_lnk());

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_sysfs_symlink)
UNITTEST("kobject_symlink_directory_contains_device_links",
         unit_testing::kobject_symlink_directory_contains_device_links)
UNITTEST_END_TESTCASE(starnix_sysfs_symlink, "starnix_sysfs_symlink",
                      "Tests for KObject Symlink Directory")
