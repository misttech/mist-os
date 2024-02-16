// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <signal.h>
#include <unistd.h>

#include <fbl/unique_fd.h>
#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

TEST(PipeTest, NonBlockingPartialWrite) {
  // Allocate 1M that should be bigger than the pipe buffer.
  constexpr ssize_t kBufferSize = 1024 * 1024;

  int pipefd[2];
  SAFE_SYSCALL(pipe2(pipefd, O_NONBLOCK));

  char* buffer = static_cast<char*>(malloc(kBufferSize));
  ASSERT_NE(buffer, nullptr);
  ssize_t write_result = write(pipefd[1], buffer, kBufferSize);
  free(buffer);
  ASSERT_GT(write_result, 0);
  ASSERT_LT(write_result, kBufferSize);
}

TEST(PipeTest, BlockingSmallWrites) {
  // Create a pipe with size 4096, and fill all but 128 bytes of it.
  int pipefd[2];
  SAFE_SYSCALL(pipe2(pipefd, O_NONBLOCK));
  SAFE_SYSCALL(fcntl(pipefd[1], F_SETPIPE_SZ, getpagesize()));
  const int kWriteSize = getpagesize() - 128;
  char buf[kWriteSize];
  ASSERT_EQ(write(pipefd[1], buf, kWriteSize), kWriteSize);
  // Trying to write 256 bytes must returns EAGAIN
  ASSERT_EQ(write(pipefd[1], buf, 256), -1);
  ASSERT_EQ(errno, EAGAIN);
}

TEST(PipeTest, SpliceShortRead) {
  char* tmp = getenv("TEST_TMPDIR");
  std::string path = tmp == nullptr ? "/tmp/test_file" : std::string(tmp) + "/test_file";
  fbl::unique_fd fd(open(path.c_str(), O_RDWR | O_CREAT | O_TRUNC, 0777));
  ASSERT_TRUE(fd.is_valid());
  ASSERT_EQ(write(fd.get(), "hello", 5), 5);
  int pipefd[2];
  SAFE_SYSCALL(pipe2(pipefd, 0));
  off64_t offset = 0;
  ASSERT_EQ(splice(fd.get(), &offset, pipefd[1], nullptr, 100, 0), 5);
  char buffer[100];
  ASSERT_EQ(read(pipefd[0], buffer, 10), 5);
  ASSERT_EQ(strncmp(buffer, "hello", 5), 0);
}

TEST(PipeTest, TeeFromEmptyPipe) {
  int pipe_a[2];
  SAFE_SYSCALL(pipe2(pipe_a, 0));
  // Closing the write end of pipe_a makes the read end readable even though it's empty.
  close(pipe_a[1]);
  int pipe_b[2];
  SAFE_SYSCALL(pipe2(pipe_b, 0));
  ASSERT_EQ(tee(pipe_a[0], pipe_b[1], 100, 0), 0);
  close(pipe_a[0]);
  close(pipe_b[0]);
  close(pipe_b[1]);
}

std::string CreateNewFifo() {
  char* tmp = getenv("TEST_TMPDIR");
  std::string dir_path = tmp == nullptr ? "/tmp/dirXXXXXX" : std::string(tmp) + "/dirXXXXXX";
  mkdtemp(&dir_path[0]);
  std::string fifo_path = dir_path + "/fifo";
  EXPECT_EQ(mkfifo(fifo_path.c_str(), 0600), 0);
  return fifo_path;
}

TEST(PipeTest, OpenFifoRW) {
  std::string path = CreateNewFifo();
  // Open and close the the fifo once.
  fbl::unique_fd fifo(open(path.c_str(), O_RDWR));
  ASSERT_TRUE(fifo.is_valid());
}

TEST(PipeTest, OpenFifoRo_NotBlocking) {
  std::string path = CreateNewFifo();
  // Reopen the fifo and check it is not disconnected.
  fbl::unique_fd fifo(open(path.c_str(), O_RDONLY | O_NONBLOCK));
  ASSERT_TRUE(fifo.is_valid());
}

void on_alarm(int) {}

TEST(PipeTest, OpenFifoBlock) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    std::string path = CreateNewFifo();
    struct sigaction act;
    act.sa_handler = on_alarm;
    act.sa_flags = 0;
    sigaction(SIGALRM, &act, nullptr);
    alarm(1);
    int fd = open(path.c_str(), O_RDONLY);
    ASSERT_EQ(fd, -1);
    ASSERT_EQ(errno, EINTR);
  });
}

}  // namespace
