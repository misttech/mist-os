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
#include <thread>

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
      fbl::unique_fd mem_fd(test_helper::MemFdCreate("try_create", O_WRONLY));
      EXPECT_TRUE(mem_fd.is_valid()) << "memfd_create: " << std::strerror(errno);
    }
  });

  EXPECT_TRUE(helper.WaitForChildren());
}

TEST(EventFdSemTest, WaitFromThread) {
  test_helper::EventFdSem sem(0);
  std::thread t1([&sem]() { SAFE_SYSCALL(sem.Wait()); });

  SAFE_SYSCALL(sem.Notify(1));
  t1.join();
}

TEST(EventFdSemTest, WaitFromFork) {
  test_helper::EventFdSem sem(0);
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([&sem]() { SAFE_SYSCALL(sem.Wait()); });

  SAFE_SYSCALL(sem.Notify(1));
  EXPECT_TRUE(helper.WaitForChildren());
}

TEST(EventFdSemTest, CheckWaitThreaded) {
  test_helper::EventFdSem sem(0);
  std::atomic<bool> flag = false;

  std::thread t1([&sem, &flag]() {
    SAFE_SYSCALL(sem.Wait());
    flag = true;
  });

  EXPECT_EQ(flag, false);
  SAFE_SYSCALL(sem.Notify(1));
  t1.join();
  EXPECT_EQ(flag, true);
}

TEST(EventFdSemTest, NotifyFromThread) {
  test_helper::EventFdSem sem(0);
  std::thread t1([&sem]() { SAFE_SYSCALL(sem.Notify(1)); });

  t1.join();
  SAFE_SYSCALL(sem.Wait());
}

TEST(EventFdSemTest, NotifyFromFork) {
  test_helper::EventFdSem sem(0);
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([&sem]() { SAFE_SYSCALL(sem.Notify(1)); });

  EXPECT_TRUE(helper.WaitForChildren());
  SAFE_SYSCALL(sem.Wait());
}

}  // namespace
