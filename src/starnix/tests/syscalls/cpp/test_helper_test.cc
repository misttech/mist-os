// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

#include <errno.h>
#include <fcntl.h>
#include <lib/fit/defer.h>
#include <sys/resource.h>
#include <unistd.h>

#include <cstring>

#include <gtest/gtest.h>

namespace {

TEST(TestHelperTest, DetectFailingChildren) {
  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([] { FAIL() << "Expected failure"; });

  EXPECT_FALSE(helper.WaitForChildren());
}

TEST(ScopedTestDirTest, DoesntLeakFileDescriptors) {
  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([] {
    struct rlimit limit;
    ASSERT_EQ(getrlimit(RLIMIT_NOFILE, &limit), 0) << "getrlimit: " << std::strerror(errno);
    auto cleanup = fit::defer([limit]() {
      EXPECT_EQ(setrlimit(RLIMIT_NOFILE, &limit), 0) << "setrlimit: " << std::strerror(errno);
    });

    struct rlimit new_limit = {.rlim_cur = 100, .rlim_max = limit.rlim_max};
    ASSERT_EQ(setrlimit(RLIMIT_NOFILE, &new_limit), 0) << "setrlimit: " << std::strerror(errno);

    for (size_t i = 0; i <= new_limit.rlim_cur; i++) {
      test_helper::ScopedTempDir scoped_dir;
      test_helper::ScopedFD mem_fd(test_helper::MemFdCreate("try_create", O_WRONLY));
      EXPECT_TRUE(mem_fd.is_valid()) << "memfd_create: " << std::strerror(errno);
    }
  });

  EXPECT_TRUE(helper.WaitForChildren());
}

}  // namespace
