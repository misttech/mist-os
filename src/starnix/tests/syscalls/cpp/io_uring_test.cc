// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/syscall.h>
#include <unistd.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>
#include <linux/io_uring.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

#ifndef IORING_SETUP_COOP_TASKRUN
#define IORING_SETUP_COOP_TASKRUN (1U << 8)
#endif

#ifndef IORING_SETUP_SINGLE_ISSUER
#define IORING_SETUP_SINGLE_ISSUER (1U << 12)
#endif

namespace {

int io_uring_setup(uint32_t entries, io_uring_params* params) {
  return static_cast<int>(syscall(__NR_io_uring_setup, entries, params));
}

TEST(IoUringTest, IoUringSetupCoopTaskrun) {
  if (!test_helper::IsStarnix() && !test_helper::IsKernelVersionAtLeast(5, 19)) {
    GTEST_SKIP() << "Skip test for unsupported feature";
  }
  struct io_uring_params params = {};
  params.flags = IORING_SETUP_COOP_TASKRUN;
  fbl::unique_fd fd(io_uring_setup(1, &params));
  ASSERT_TRUE(fd.is_valid()) << strerror(errno);
}

TEST(IoUringTest, IoUringSetupSingleIssuer) {
  if (!test_helper::IsStarnix() && !test_helper::IsKernelVersionAtLeast(6, 0)) {
    GTEST_SKIP() << "Skip test for unsupported feature";
  }
  struct io_uring_params params = {};
  params.flags = IORING_SETUP_SINGLE_ISSUER;
  fbl::unique_fd fd(io_uring_setup(1, &params));
  ASSERT_TRUE(fd.is_valid()) << strerror(errno);
}

}  // namespace
