// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "test-chdir-guard.h"

#include <fcntl.h>
#include <lib/elfldltl/testing/get-test-data.h>
#include <unistd.h>

#include <cerrno>
#include <cstring>

#include <gtest/gtest.h>

namespace ld::testing {

TestChdirGuard::TestChdirGuard() {
  cwd_ = open(".", O_RDONLY | O_DIRECTORY);
  EXPECT_GE(cwd_, 0) << strerror(errno);
  if (cwd_ >= 0) {
    std::filesystem::path path = elfldltl::testing::GetTestDataPath(".");
    EXPECT_FALSE(path.empty());
    EXPECT_EQ(chdir(path.c_str()), 0) << path << ": " << strerror(errno);
  }
}

TestChdirGuard::~TestChdirGuard() {
  if (cwd_ >= 0) {
    EXPECT_EQ(fchdir(cwd_), 0) << strerror(errno);
    EXPECT_EQ(close(cwd_), 0) << strerror(errno);
  }
}

}  // namespace ld::testing
