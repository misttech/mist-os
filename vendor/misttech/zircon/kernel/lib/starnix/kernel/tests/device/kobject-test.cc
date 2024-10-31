// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/device/kobject.h>
#include <lib/mistos/starnix/kernel/fs/sysfs/kobject_directory.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node_ops.h>
#include <lib/mistos/util/testing/unittest.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {

using starnix::FsNodeOps;
using starnix::FsString;
using starnix::KObject;
using starnix::KObjectDirectory;

namespace {

bool kobject_create_child() {
  BEGIN_TEST;
  auto root = KObject::new_root(FsString());
  ASSERT_FALSE(root->parent().has_value());
  ASSERT_FALSE(root->has_child(FsString("virtual")));
  root->get_or_create_child<FsNodeOps>(FsString("virtual"), KObjectDirectory::New);
  EXPECT_TRUE(root->has_child(FsString("virtual")));
  END_TEST;
}

bool kobject_path() {
  BEGIN_TEST;
  auto root = KObject::new_root(FsString("devices"));
  auto bus = root->get_or_create_child<FsNodeOps>(FsString("virtual"), KObjectDirectory::New);
  auto device = bus->get_or_create_child<FsNodeOps>(FsString("mem"), KObjectDirectory::New)
                    ->get_or_create_child<FsNodeOps>(FsString("null"), KObjectDirectory::New);
  ASSERT_STREQ("devices/virtual/mem/null", device->path().c_str());
  END_TEST;
}

bool kobject_path_to_root() {
  BEGIN_TEST;
  auto root = KObject::new_root(FsString());
  auto bus = root->get_or_create_child<FsNodeOps>(FsString("bus"), KObjectDirectory::New);
  auto device = bus->get_or_create_child<FsNodeOps>(FsString("device"), KObjectDirectory::New);
  ASSERT_STREQ("../..", device->path_to_root().c_str());
  END_TEST;
}

bool kobject_get_children_names() {
  BEGIN_TEST;
  auto root = KObject::new_root(FsString());
  root->get_or_create_child<FsNodeOps>(FsString("virtual"), KObjectDirectory::New);
  root->get_or_create_child<FsNodeOps>(FsString("cpu"), KObjectDirectory::New);
  root->get_or_create_child<FsNodeOps>(FsString("power"), KObjectDirectory::New);

  auto names = root->get_children_names();
  bool has_virtual = false;
  bool has_cpu = false;
  bool has_power = false;
  bool has_system = false;
  for (const auto& name : names) {
    if (name == FsString("virtual"))
      has_virtual = true;
    if (name == FsString("cpu"))
      has_cpu = true;
    if (name == FsString("power"))
      has_power = true;
    if (name == FsString("system"))
      has_system = true;
  }
  ASSERT_TRUE(has_virtual);
  ASSERT_TRUE(has_cpu);
  ASSERT_TRUE(has_power);
  ASSERT_FALSE(has_system);
  END_TEST;
}

bool kobject_remove() {
  BEGIN_TEST;
  auto root = KObject::new_root(FsString());
  auto bus = root->get_or_create_child<FsNodeOps>(FsString("virtual"), KObjectDirectory::New);
  auto class_obj = bus->get_or_create_child<FsNodeOps>(FsString("mem"), KObjectDirectory::New);
  ASSERT_TRUE(bus->has_child(FsString("mem")));
  class_obj->remove();
  ASSERT_FALSE(bus->has_child(FsString("mem")));
  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_device_kobject)
UNITTEST("kobject create child", unit_testing::kobject_create_child)
UNITTEST("kobject path", unit_testing::kobject_path)
UNITTEST("kobject_path_to_root", unit_testing::kobject_path_to_root)
UNITTEST("kobject get children names", unit_testing::kobject_get_children_names)
UNITTEST("kobject remove", unit_testing::kobject_remove)
UNITTEST_END_TESTCASE(starnix_device_kobject, "starnix_device_kobject", "Tests for KObject")
