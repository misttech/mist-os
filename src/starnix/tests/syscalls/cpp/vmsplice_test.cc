// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <unistd.h>

#include <cstdlib>
#include <string>

#include <gtest/gtest.h>

namespace {

TEST(VmspliceTest, VmspliceReadClosedPipe) {
  int fds[2];
  ASSERT_EQ(pipe(fds), 0);
  ASSERT_EQ(close(fds[1]), 0);
  char buffer[4096] = {};
  iovec iov = {
      .iov_base = buffer,
      .iov_len = sizeof(buffer),
  };
  ASSERT_EQ(vmsplice(fds[0], &iov, 1, SPLICE_F_NONBLOCK), 0);
  ASSERT_EQ(vmsplice(fds[0], &iov, 1, 0), 0);
  close(fds[0]);
}

TEST(VmspliceTest, VmspliceWriteClosedPipe) {
  EXPECT_EXIT(([]() {
                signal(SIGPIPE, SIG_IGN);

                int fds[2];
                ASSERT_EQ(pipe(fds), 0);
                ASSERT_EQ(close(fds[0]), 0);
                char buffer[4096] = {};
                iovec iov = {
                    .iov_base = buffer,
                    .iov_len = sizeof(buffer),
                };
                ASSERT_EQ(vmsplice(fds[1], &iov, 1, SPLICE_F_NONBLOCK), -1);
                ASSERT_EQ(errno, EPIPE);
                ASSERT_EQ(vmsplice(fds[1], &iov, 1, 0), -1);
                ASSERT_EQ(errno, EPIPE);
                close(fds[1]);
                exit(42);
              })(),
              testing::ExitedWithCode(42), "");
}

}  // namespace
