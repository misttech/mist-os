// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/syscall.h>
#include <unistd.h>

#include <thread>

#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

// TODO(fxbug.dev/135633): This define will no longer be needed when x64 and arm64 switch to the
// ubuntu20.04 sysroot too.
#ifndef __riscv
#define SYS_pidfd_open 434
#endif

// Our Linux sysroot doesn't seem to have pidfd_open() and gettid().
int DoPidFdOpen(pid_t pid) { return static_cast<int>(syscall(SYS_pidfd_open, pid, 0u)); }
pid_t DoGetTid() { return static_cast<pid_t>(syscall(SYS_gettid)); }

TEST(PidFdTest, ProcessCanBeOpened) {
  int pid_fd = DoPidFdOpen(getpid());
  ASSERT_GE(pid_fd, 0);
  close(pid_fd);
}

TEST(PidFdTest, ThreadCannotBeOpened) {
  std::thread([] {
    int pid_fd = DoPidFdOpen(DoGetTid());
    ASSERT_EQ(pid_fd, -1);
    EXPECT_EQ(errno, EINVAL);
  }).join();
}

}  // namespace
