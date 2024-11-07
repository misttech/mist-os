// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/fs/sysfs/bus_collection_directory.h>
#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/vfs/lookup_context.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/mistos/util/weak_wrapper.h>
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

bool bus_collection_directory_contains_expected_files() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  auto root_kobject = starnix::KObject::new_root("");
  root_kobject->get_or_create_child<FsNodeOps>("0", starnix::KObjectDirectory::New);
  auto test_fs = starnix::testing::create_fs(
      kernel, starnix::BusCollectionDirectory::New(util::WeakPtr(root_kobject.get())));

  auto device_entry = lookup_node(*current_task, test_fs, "devices");
  ASSERT_TRUE(device_entry.is_ok(), "devices");
  // TODO(b/297369112): uncomment when "drivers" are added.
  // lookup_node(&current_task, &test_fs, b"drivers").expect("drivers");

  END_TEST;
}

bool bus_devices_directory_contains_device_links() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  auto root_kobject = starnix::KObject::new_root("");
  root_kobject->get_or_create_child<FsNodeOps>("0", starnix::KObjectDirectory::New);
  auto test_fs = starnix::testing::create_fs(
      kernel, starnix::BusCollectionDirectory::New(util::WeakPtr(root_kobject.get())));

  auto device_entry = lookup_node(*current_task, test_fs, "devices/0");
  ASSERT_TRUE(device_entry.is_ok(), "device 0 directory");
  ASSERT_TRUE(device_entry->entry_->node_->is_lnk());

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_sysfs_bus_collection)
UNITTEST("bus collection_ directory contains expected files",
         unit_testing::bus_collection_directory_contains_expected_files)
UNITTEST("bus devices directory contains device links",
         unit_testing::bus_devices_directory_contains_device_links)
UNITTEST_END_TESTCASE(starnix_sysfs_bus_collection, "starnix_sysfs_bus_collection",
                      "Tests for Sysfs Bus Collection")
