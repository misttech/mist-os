// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/fs/tmpfs.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/vfs/dir_entry.h>
#include <lib/mistos/starnix/kernel/vfs/lookup_context.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/mistos/util/testing/unittest.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {
namespace {

using starnix::LookupContext;
using starnix::Namespace;
using starnix::NamespaceNode;
using starnix::RenameFlags;
using starnix::TmpFs;
using starnix::UnlinkKind;
using starnix::WhatToMount;

bool test_namespace() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  auto root_fs = TmpFs::new_fs(kernel);
  auto root_node = root_fs->root();
  auto _dev_node = root_node->create_dir(*current_task, "dev");
  ASSERT_TRUE(_dev_node.is_ok(), "failed to mkdir dev");
  auto dev_fs = TmpFs::new_fs(kernel);
  auto dev_root_node = dev_fs->root();
  auto _dev_pts_node = dev_root_node->create_dir(*current_task, "pts");
  ASSERT_TRUE(_dev_pts_node.is_ok(), "failed to mkdir pts");

  auto ns = Namespace::New(root_fs);
  auto context = LookupContext::Default();
  auto dev = ns->root().lookup_child(*current_task, context, "dev");
  ASSERT_TRUE(dev.is_ok(), "failed to lookup dev");

  ASSERT_TRUE(dev->mount(WhatToMount::Fs(dev_fs), MountFlags::empty()).is_ok(),
              "failed to mount dev root node");

  context = LookupContext::Default();
  dev = ns->root().lookup_child(*current_task, context, "dev");
  ASSERT_TRUE(dev.is_ok(), "failed to lookup dev again");

  context = LookupContext::Default();
  auto pts = dev->lookup_child(*current_task, context, "pts");
  ASSERT_TRUE(pts.is_ok(), "failed to lookup pts");
  auto pts_parent = pts.value().parent();
  ASSERT_TRUE(pts_parent.has_value(), "failed to get parent of pts");
  ASSERT_TRUE(pts_parent->entry_ == dev->entry_);

  auto dev_parent = dev.value().parent();
  ASSERT_TRUE(dev_parent.has_value(), "failed to get parent of dev");
  ASSERT_TRUE(dev_parent->entry_ == ns->root().entry_);
  END_TEST;
}

bool test_mount_does_not_upgrade() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  auto root_fs = TmpFs::new_fs(kernel);
  auto root_node = root_fs->root();
  auto _dev_node = root_node->create_dir(*current_task, "dev");
  ASSERT_TRUE(_dev_node.is_ok(), "failed to mkdir dev");
  auto dev_fs = TmpFs::new_fs(kernel);
  auto dev_root_node = dev_fs->root();
  auto _dev_pts_node = dev_root_node->create_dir(*current_task, "pts");
  ASSERT_TRUE(_dev_pts_node.is_ok(), "failed to mkdir pts");

  auto ns = Namespace::New(root_fs);
  auto context = LookupContext::Default();
  auto dev = ns->root().lookup_child(*current_task, context, "dev");
  ASSERT_TRUE(dev.is_ok(), "failed to lookup dev");

  ASSERT_TRUE(dev->mount(WhatToMount::Fs(dev_fs), MountFlags::empty()).is_ok(),
              "failed to mount dev root node");

  context = LookupContext::Default();
  auto new_dev = ns->root().lookup_child(*current_task, context, "dev");
  ASSERT_TRUE(new_dev.is_ok(), "failed to lookup dev again");
  ASSERT_TRUE(dev->entry_ != new_dev->entry_);

  context = LookupContext::Default();
  auto new_pts = new_dev->lookup_child(*current_task, context, "pts");
  ASSERT_TRUE(new_pts.is_ok(), "failed to lookup pts");

  context = LookupContext::Default();
  auto old_pts = dev->lookup_child(*current_task, context, "pts");
  ASSERT_TRUE(old_pts.is_error());

  END_TEST;
}

bool test_path() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  auto root_fs = TmpFs::new_fs(kernel);
  auto root_node = root_fs->root();
  auto _dev_node = root_node->create_dir(*current_task, "dev");
  ASSERT_TRUE(_dev_node.is_ok(), "failed to mkdir dev");
  auto dev_fs = TmpFs::new_fs(kernel);
  auto dev_root_node = dev_fs->root();
  auto _dev_pts_node = dev_root_node->create_dir(*current_task, "pts");
  ASSERT_TRUE(_dev_pts_node.is_ok(), "failed to mkdir pts");

  auto ns = Namespace::New(root_fs);
  auto context = LookupContext::Default();
  auto dev = ns->root().lookup_child(*current_task, context, "dev");
  ASSERT_TRUE(dev.is_ok(), "failed to lookup dev");

  ASSERT_TRUE(dev->mount(WhatToMount::Fs(dev_fs), MountFlags::empty()).is_ok(),
              "failed to mount dev root node");

  context = LookupContext::Default();
  auto dev_again = ns->root().lookup_child(*current_task, context, "dev");
  ASSERT_TRUE(dev_again.is_ok(), "failed to lookup dev");

  context = LookupContext::Default();
  auto pts = dev_again->lookup_child(*current_task, context, "pts");
  ASSERT_TRUE(pts.is_ok(), "failed to lookup pts");

  ASSERT_STREQ("/", ns->root().path_escaping_chroot());
  ASSERT_STREQ("/dev", dev_again->path_escaping_chroot());
  ASSERT_STREQ("/dev/pts", pts->path_escaping_chroot());

  END_TEST;
}

bool test_shadowing() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  auto root_fs = TmpFs::new_fs(kernel);
  auto ns = Namespace::New(root_fs);
  auto _foo_node = root_fs->root()->create_dir(*current_task, "foo");
  ASSERT_TRUE(_foo_node.is_ok(), "failed to create foo dir");

  auto context = LookupContext::Default();
  auto foo_dir = ns->root().lookup_child(*current_task, context, "foo");
  ASSERT_TRUE(foo_dir.is_ok(), "failed to lookup foo");

  auto foofs1 = TmpFs::new_fs(kernel);
  ASSERT_TRUE(foo_dir->mount(WhatToMount::Fs(foofs1), MountFlags::empty()).is_ok(),
              "failed to mount foofs1");

  context = LookupContext::Default();
  auto foo_lookup1 = ns->root().lookup_child(*current_task, context, "foo");
  ASSERT_TRUE(foo_lookup1.is_ok(), "failed to lookup foo after first mount");
  ASSERT_TRUE(foo_lookup1->entry_ == foofs1->root(), "foo should point to foofs1 root");

  context = LookupContext::Default();
  auto foo_dir2 = ns->root().lookup_child(*current_task, context, "foo");
  ASSERT_TRUE(foo_dir2.is_ok(), "failed to lookup foo again");

  auto ns_clone = ns->clone_namespace();

  auto foofs2 = TmpFs::new_fs(kernel);
  ASSERT_TRUE(foo_dir2->mount(WhatToMount::Fs(foofs2), MountFlags::empty()).is_ok(),
              "failed to mount foofs2");

  context = LookupContext::Default();
  auto foo_lookup2 = ns->root().lookup_child(*current_task, context, "foo");
  ASSERT_TRUE(foo_lookup2.is_ok(), "failed to lookup foo after second mount");
  ASSERT_TRUE(foo_lookup2->entry_ == foofs2->root(), "foo should point to foofs2 root");

  context = LookupContext::Default();
  auto foo_lookup_clone = ns_clone->root().lookup_child(*current_task, context, "foo");
  ASSERT_TRUE(foo_lookup_clone.is_ok(), "failed to lookup foo in clone");
  ASSERT_TRUE(foo_lookup_clone->entry_ == foofs1->root(),
              "foo in clone should still point to foofs1 root");

  END_TEST;
}

bool test_unlink_mounted_directory() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  auto root_fs = TmpFs::new_fs(kernel);
  auto ns1 = Namespace::New(root_fs);
  auto ns2 = Namespace::New(root_fs);
  auto _foo_node = root_fs->root()->create_dir(*current_task, "foo");
  ASSERT_TRUE(_foo_node.is_ok(), "failed to create foo dir");
  auto context = LookupContext::Default();
  auto foo_dir = ns1->root().lookup_child(*current_task, context, "foo");
  ASSERT_TRUE(foo_dir.is_ok(), "failed to lookup foo");

  auto foofs = TmpFs::new_fs(kernel);
  ASSERT_TRUE(foo_dir->mount(WhatToMount::Fs(foofs), MountFlags::empty()).is_ok(),
              "failed to mount foofs");

  // Trying to unlink from ns1 should fail
  auto unlink_result1 = ns1->root().unlink(*current_task, "foo", UnlinkKind::Directory, false);
  ASSERT_TRUE(unlink_result1.is_error(), "unlink from ns1 should fail");
  ASSERT_EQ(errno(EBUSY).error_code(), unlink_result1.error_value().error_code(),
            "wrong error code");

  // But unlinking from ns2 should succeed
  auto unlink_result2 = ns2->root().unlink(*current_task, "foo", UnlinkKind::Directory, false);
  ASSERT_TRUE(unlink_result2.is_ok(), "unlink from ns2 failed");

  // And it should no longer show up in ns1
  auto unlink_result3 = ns1->root().unlink(*current_task, "foo", UnlinkKind::Directory, false);
  ASSERT_TRUE(unlink_result3.is_error(), "unlink from ns1 should fail");
  ASSERT_EQ(errno(ENOENT).error_code(), unlink_result3.error_value().error_code(),
            "wrong error code");

  END_TEST;
}

bool test_rename_mounted_directory() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  auto root_fs = TmpFs::new_fs(kernel);
  auto ns1 = Namespace::New(root_fs);
  auto ns2 = Namespace::New(root_fs);
  auto _foo_node = root_fs->root()->create_dir(*current_task, "foo");
  ASSERT_TRUE(_foo_node.is_ok(), "failed to create foo dir");
  auto _bar_node = root_fs->root()->create_dir(*current_task, "bar");
  ASSERT_TRUE(_bar_node.is_ok(), "failed to create bar dir");
  auto _baz_node = root_fs->root()->create_dir(*current_task, "baz");
  ASSERT_TRUE(_baz_node.is_ok(), "failed to create baz dir");

  auto context = LookupContext::Default();
  auto foo_dir = ns1->root().lookup_child(*current_task, context, "foo");
  ASSERT_TRUE(foo_dir.is_ok(), "failed to lookup foo");

  auto foofs = TmpFs::new_fs(kernel);
  ASSERT_TRUE(foo_dir->mount(WhatToMount::Fs(foofs), MountFlags::empty()).is_ok(),
              "failed to mount foofs");

  // Trying to rename over foo from ns1 should fail
  auto rename_result1 = NamespaceNode::rename(*current_task, ns1->root(), "bar", ns1->root(), "foo",
                                              RenameFlags::empty());
  ASSERT_TRUE(rename_result1.is_error(), "rename to foo from ns1 should fail");
  ASSERT_EQ(errno(EBUSY).error_code(), rename_result1.error_value().error_code(),
            "wrong error code");

  // Likewise the other way
  auto rename_result2 = NamespaceNode::rename(*current_task, ns1->root(), "foo", ns1->root(), "bar",
                                              RenameFlags::empty());
  ASSERT_TRUE(rename_result2.is_error(), "rename from foo in ns1 should fail");
  ASSERT_EQ(errno(EBUSY).error_code(), rename_result2.error_value().error_code(),
            "wrong error code");

  // But renaming from ns2 should succeed.
  auto root = ns2->root();
  auto rename_result3 =
      NamespaceNode::rename(*current_task, root, "foo", root, "bar", RenameFlags::empty());
  ASSERT_TRUE(rename_result3.is_ok(), "rename in ns2 failed");

  // Renaming over a directory with a mount should also work
  auto rename_result4 =
      NamespaceNode::rename(*current_task, root, "baz", root, "bar", RenameFlags::empty());
  ASSERT_TRUE(rename_result4.is_ok(), "rename over mounted dir failed");

  // "foo" and "baz" should no longer show up in ns1
  auto foo_lookup = ns1->root().lookup_child(*current_task, context, "foo");
  ASSERT_TRUE(foo_lookup.is_error(), "foo should not exist");
  ASSERT_EQ(errno(ENOENT).error_code(), foo_lookup.error_value().error_code(), "wrong error code");

  auto baz_lookup = ns1->root().lookup_child(*current_task, context, "baz");
  ASSERT_TRUE(baz_lookup.is_error(), "baz should not exist");
  ASSERT_EQ(errno(ENOENT).error_code(), baz_lookup.error_value().error_code(), "wrong error code");

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_vfs_namespace)
UNITTEST("test namespace", unit_testing::test_namespace)
UNITTEST("test mount does not upgrade", unit_testing::test_mount_does_not_upgrade)
UNITTEST("test path", unit_testing::test_path)
UNITTEST("test shadowing", unit_testing::test_shadowing)
UNITTEST("test unlink mounted directory", unit_testing::test_unlink_mounted_directory)
UNITTEST("test rename mounted directory", unit_testing::test_rename_mounted_directory)
UNITTEST_END_TESTCASE(starnix_vfs_namespace, "starnix_vfs_namespace", "Tests for VFS Namespace")
