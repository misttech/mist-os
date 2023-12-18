// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <unistd.h>

#include <gtest/gtest.h>
#include <linux/ashmem.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

class AshmemTest : public ::testing::Test {
  void SetUp() override {
    // /dev/ashmem must exist
    if (access("/dev/ashmem", F_OK) != 0) {
      GTEST_SKIP() << "/dev/ashmem is not present.";
    }
  }
};

TEST_F(AshmemTest, Open) {
  test_helper::ScopedFD ashmem_fd(open("/dev/ashmem", O_RDWR));
  ASSERT_TRUE(ashmem_fd.is_valid());
}

}  // namespace
