// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/fs/mistos/tree_builder.h>
#include <lib/mistos/starnix/kernel/fs/tmpfs.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/vfs/fs_node.h>
#include <lib/mistos/starnix/kernel/vfs/memory_file.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/unittest/unittest.h>

#include <fbl/alloc_checker.h>
#include <ktl/unique_ptr.h>
#include <object/handle.h>

namespace unit_testing {

using namespace starnix;

bool two_files() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();

  auto fs = TmpFs::new_fs(kernel);

  auto tree = TreeBuilder::empty_dir();
  fbl::AllocChecker ac;
  fbl::Vector<ktl::string_view> path_a;
  fbl::Vector<ktl::string_view> path_b;

  path_a.push_back("a", &ac);
  ASSERT(ac.check());

  path_b.push_back("b", &ac);
  ASSERT(ac.check());

  ASSERT_TRUE(
      tree.add_entry(path_a, ktl::move(ktl::unique_ptr<FsNodeOps>(MemoryFileNode::New().value())))
          .is_ok());
  ASSERT_TRUE(
      tree.add_entry(path_b, ktl::move(ktl::unique_ptr<FsNodeOps>(MemoryFileNode::New().value())))
          .is_ok());

  auto root = tree.build(fs);
  ASSERT_NONNULL(root);

  END_TEST;
}

bool overlapping_paths() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();

  auto fs = TmpFs::new_fs(kernel);

  auto tree = TreeBuilder::empty_dir();
  fbl::AllocChecker ac;
  fbl::Vector<ktl::string_view> path_a;
  fbl::Vector<ktl::string_view> path_b;
  fbl::Vector<ktl::string_view> path_c;

  // /one/two
  path_a.push_back("one", &ac);
  ASSERT(ac.check());
  path_a.push_back("two", &ac);
  ASSERT(ac.check());

  // /one/three
  path_b.push_back("one", &ac);
  ASSERT(ac.check());
  path_b.push_back("three", &ac);
  ASSERT(ac.check());

  // /four
  path_c.push_back("four", &ac);
  ASSERT(ac.check());

  ASSERT_TRUE(
      tree.add_entry(path_a, ktl::move(ktl::unique_ptr<FsNodeOps>(MemoryFileNode::New().value())))
          .is_ok());
  ASSERT_TRUE(
      tree.add_entry(path_b, ktl::move(ktl::unique_ptr<FsNodeOps>(MemoryFileNode::New().value())))
          .is_ok());
  ASSERT_TRUE(
      tree.add_entry(path_c, ktl::move(ktl::unique_ptr<FsNodeOps>(MemoryFileNode::New().value())))
          .is_ok());

  auto root = tree.build(fs);
  ASSERT_NONNULL(root);

  END_TEST;
}

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_fs_treebuilder)
UNITTEST("test two_files", unit_testing::two_files)
UNITTEST("test overlapping_paths", unit_testing::overlapping_paths)
UNITTEST_END_TESTCASE(starnix_fs_treebuilder, "starnix_fs_treebuilder", "Tests for starnix bootfs")
