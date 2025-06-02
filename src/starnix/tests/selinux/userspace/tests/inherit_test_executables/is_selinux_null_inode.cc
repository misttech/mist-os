// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <gtest/gtest.h>

#include "src/starnix/tests/selinux/userspace/util.h"
#include "stdio.h"
#include "stdlib.h"

int main(int argc, char** argv) {
  int fd = atoi(argv[1]);
  bool expect_null_inode = atoi(argv[2]);
  EXPECT_EQ(IsSelinuxNullInode(fd), fit::ok(expect_null_inode));
  return ::testing::Test::HasFailure() ? 1 : 0;
}
