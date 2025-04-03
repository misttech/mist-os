// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/fit/defer.h>
#include <sys/poll.h>
#include <sys/syscall.h>
#include <unistd.h>

#include <thread>

#include <gtest/gtest.h>

#include "src/lib/files/file.h"
#include "src/lib/fxl/strings/string_number_conversions.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

// Our Linux sysroot doesn't seem to have pidfd_open() and gettid().
int DoPidFdOpen(pid_t pid) { return static_cast<int>(syscall(SYS_pidfd_open, pid, 0u)); }
pid_t DoGetTid() { return static_cast<pid_t>(syscall(SYS_gettid)); }

// Returns (readable_end, writable_end).
std::pair<int, int> CreatePipe() {
  int pipe_fds[2];
  SAFE_SYSCALL(pipe(pipe_fds));
  return {pipe_fds[0], pipe_fds[1]};
}

// Waits until the given process' main thread becomes a zombie.
void WaitUntilMainThreadIsZombie(pid_t pid) {
  std::string stat_path =
      "/proc/" + fxl::NumberToString(pid) + "/task/" + fxl::NumberToString(pid) + "/stat";

  while (true) {
    std::string contents;
    ASSERT_TRUE(files::ReadFileToString(stat_path, &contents));

    char state;
    ASSERT_EQ(sscanf(contents.c_str(), "%*d %*s %c", &state), 1) << contents;

    if (state == 'Z') {
      return;  // thread is a Zombie, we're done waiting!
    }

    usleep(10000);  // check again in 10 ms.
  }
}

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

TEST(PidFdTest, CanPollProcessExit) {
  // Create a pipe that will be used to ask the child process to exit.
  auto [r_fd, w_fd] = CreatePipe();

  test_helper::ForkHelper helper;
  pid_t pid = helper.RunInForkedProcess([r_fd = r_fd, w_fd = w_fd] {
    close(w_fd);

    // Wait for the readable end to signal the end of the stream.
    char buf;
    read(r_fd, &buf, 1);

    _exit(0);
  });

  close(r_fd);

  int pid_fd = SAFE_SYSCALL(DoPidFdOpen(pid));

  // Verify that poll does not return POLLIN while the process is running.
  pollfd pfd = {.fd = pid_fd, .events = POLLIN};
  ASSERT_EQ(poll(&pfd, 1, 0), 0);

  // Verify that poll returns POLLIN when the process stops running.
  {
    // Do not let SIGCHLD interrupt our poll() call.
    test_helper::SignalMaskHelper signal_mask_helper;
    signal_mask_helper.blockSignal(SIGCHLD);
    auto restorer = fit::defer([&]() { signal_mask_helper.restoreSigmask(); });

    close(w_fd);
    ASSERT_EQ(poll(&pfd, 1, -1), 1);
    EXPECT_EQ(pfd.revents, POLLIN);
  }

  // Verify that poll returns POLLIN even if the wait starts after the process has exited.
  ASSERT_EQ(poll(&pfd, 1, -1), 1);
  EXPECT_EQ(pfd.revents, POLLIN);

  close(pid_fd);
  helper.WaitForChildren();
}

TEST(PidFdTest, PollWaitsForSecondaryThreadsToo) {
  // Create a pipe that will be used to ask the child process to exit.
  auto [r_fd, w_fd] = CreatePipe();

  test_helper::ForkHelper helper;
  pid_t pid = helper.RunInForkedProcess([r_fd = r_fd, w_fd = w_fd] {
    close(w_fd);

    std::thread([r_fd] {
      // Wait for the readable end to signal the end of the stream.
      char buf;
      read(r_fd, &buf, 1);
    }).detach();

    // Immediately exit the main thread, leaving the secondary thread running.
    syscall(SYS_exit, 0);
  });

  close(r_fd);

  // Wait for the main thread to exit.
  ASSERT_NO_FATAL_FAILURE(WaitUntilMainThreadIsZombie(pid));

  // Open a pidfd using the main thread's pid.
  int pid_fd = DoPidFdOpen(pid);
  ASSERT_GE(pid_fd, 0);

  // Verify that poll does not return POLLIN even after the main thread exited,
  // if a secondary thread is still running.
  pollfd pfd = {.fd = pid_fd, .events = POLLIN};
  ASSERT_EQ(poll(&pfd, 1, 500), 0);

  // Verify that poll returns POLLIN when the secondary thread stops running.
  {
    // Do not let SIGCHLD interrupt our poll() call.
    test_helper::SignalMaskHelper signal_mask_helper;
    signal_mask_helper.blockSignal(SIGCHLD);
    auto restorer = fit::defer([&]() { signal_mask_helper.restoreSigmask(); });

    close(w_fd);
    ASSERT_EQ(poll(&pfd, 1, -1), 1);
    EXPECT_EQ(pfd.revents, POLLIN);
  }

  close(pid_fd);
  helper.WaitForChildren();
}

}  // namespace
