// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <stdlib.h>
#include <sys/socket.h>
#include <unistd.h>

#include <cstdlib>
#include <string>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

namespace {

constexpr size_t kNumPipeEndsCount = 2;

void MakePipe(fbl::unique_fd (&fds)[kNumPipeEndsCount]) {
  int raw_fds[kNumPipeEndsCount] = {};
  ASSERT_EQ(pipe(raw_fds), 0) << strerror(errno);
  for (size_t i = 0; i < kNumPipeEndsCount; ++i) {
    fds[i] = fbl::unique_fd(raw_fds[i]);
    ASSERT_TRUE(fds[i]);
  }
}

TEST(VmspliceTest, VmspliceReadClosedPipe) {
  fbl::unique_fd fds[kNumPipeEndsCount];
  ASSERT_NO_FATAL_FAILURE(MakePipe(fds));
  ASSERT_EQ(fds[1].reset(), 0) << strerror(errno);
  char buffer[4096] = {};
  iovec iov = {
      .iov_base = buffer,
      .iov_len = sizeof(buffer),
  };
  ASSERT_EQ(vmsplice(fds[0].get(), &iov, 1, SPLICE_F_NONBLOCK), 0) << strerror(errno);
  ASSERT_EQ(vmsplice(fds[0].get(), &iov, 1, 0), 0) << strerror(errno);
}

TEST(VmspliceTest, VmspliceWriteClosedPipe) {
  constexpr int kRetCode = 42;
  EXPECT_EXIT(([]() {
                signal(SIGPIPE, SIG_IGN);

                fbl::unique_fd fds[kNumPipeEndsCount];
                ASSERT_NO_FATAL_FAILURE(MakePipe(fds));
                ASSERT_EQ(fds[0].reset(), 0) << strerror(errno);
                char buffer[4096] = {};
                iovec iov = {
                    .iov_base = buffer,
                    .iov_len = sizeof(buffer),
                };
                ASSERT_EQ(vmsplice(fds[1].get(), &iov, 1, SPLICE_F_NONBLOCK), -1);
                ASSERT_EQ(errno, EPIPE);
                ASSERT_EQ(vmsplice(fds[1].get(), &iov, 1, 0), -1);
                ASSERT_EQ(errno, EPIPE);
                exit(kRetCode);
              })(),
              testing::ExitedWithCode(kRetCode), "");
}

}  // namespace
