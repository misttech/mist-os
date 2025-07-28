// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>

#include <gtest/gtest.h>

#include "src/storage/f2fs/f2fs.h"
#include "src/storage/f2fs/test/unit/unit_lib.h"
#include "src/storage/f2fs/vnode.h"

namespace f2fs {
namespace {

class SimpleIOTest : public F2fsFakeDevTestFixture {
 public:
  SimpleIOTest()
      : F2fsFakeDevTestFixture(
            TestOptions{.block_count = kSectorCount100MiB, .image_file = "simple_io.img.zst"}) {}
};

TEST_F(SimpleIOTest, SimpleIO) {
  fbl::RefPtr<fs::Vnode> vnode;
  FileTester::Lookup(root_dir_.get(), "test", &vnode);
  auto test_file = fbl::RefPtr<File>::Downcast(std::move(vnode));

  std::string target_string("hello");

  auto r_buf = std::make_unique<char[]>(kPageSize);
  FileTester::ReadFromFile(test_file.get(), r_buf.get(), target_string.length() + 1, 0);
  std::string str(r_buf.get(), target_string.length());
  ASSERT_EQ(str, target_string);

  test_file->Close();
}

}  // namespace
}  // namespace f2fs
