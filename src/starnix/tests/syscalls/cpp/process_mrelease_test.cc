// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <errno.h>
#include <signal.h>
#include <string.h>
#include <sys/mman.h>
#include <sys/ptrace.h>
#include <sys/syscall.h>
#include <sys/wait.h>
#include <unistd.h>

#include <format>
#include <fstream>

#include <gtest/gtest.h>

#include "src/lib/files/file.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

// Define SYS_process_mrelease if not available in the toolchain's headers.
// This is syscall 448 on x86_64 and arm64.
#ifndef SYS_process_mrelease
#define SYS_process_mrelease 448
#endif

namespace {
const size_t kMemSizeBytes = 1 * 1024 * 1024;  // 1MB
constexpr char RSS_PREFIX[] = "VmRSS:";

int DoPidFdOpen(pid_t pid) { return static_cast<int>(syscall(SYS_pidfd_open, pid, 0u)); }
int DoProcessMRelease(int pidfd, int flags = 0) {
  return static_cast<int>(syscall(SYS_process_mrelease, pidfd, flags));
}

int GetProcessRSSMemorySize(pid_t pid) {
  auto path = std::format("/proc/{}/status", pid);
  std::ifstream file(path);
  if (!file.is_open()) {
    return testing::AssertionFailure() << "Unable to open " << path;
  }

  std::string line;
  while (std::getline(file, line)) {
    if (line.starts_with(RSS_PREFIX)) {
      std::string value_str = line.substr(strlen(RSS_PREFIX));
      // Trim leading whitespace
      value_str.erase(0, value_str.find_first_not_of(" \t\n\r\f\v"));
      // Remove " kB" suffix
      size_t kb_pos = value_str.rfind(" kB");
      if (kb_pos != std::string::npos) {
        value_str.erase(kb_pos);
      }
      return std::atoi(value_str.c_str());
    }
  }

  return testing::AssertionFailure() << "Did not find rss value";
}

std::string GetMapsString(pid_t pid) {
  auto path = std::format("/proc/{}/maps", pid);
  std::string res;
  files::ReadFileToString(path, &res);
  return res;
}

}  // namespace

class ProcessMemoryReleaseTest : public ::testing::Test {
 public:
  void SetUp() override {
    if (!test_helper::IsStarnix() && !test_helper::IsKernelVersionAtLeast(5, 15)) {
      GTEST_SKIP()
          << "process_mrelease isn't supported on Linux with kernel version older than 5.15,"
          << "skipping.";
    }
  }
};

TEST_F(ProcessMemoryReleaseTest, InvalidPidfd) {
  int invalid_pidfd = -1;
  EXPECT_THAT(DoProcessMRelease(invalid_pidfd), SyscallFailsWithErrno(EBADF));
}

TEST_F(ProcessMemoryReleaseTest, InvalidFlags) {
  test_helper::ForkHelper fork_helper;
  pid_t child_pid = fork_helper.RunInForkedProcess([]() {
    // Wait to be killed by the parent.
    pause();
  });
  int child_pidfd = DoPidFdOpen(child_pid);
  ASSERT_THAT(child_pidfd, SyscallSucceeds());
  ASSERT_THAT(kill(child_pid, SIGKILL), SyscallSucceeds());
  EXPECT_THAT(DoProcessMRelease(child_pidfd, -1), SyscallFailsWithErrno(EINVAL));
  ASSERT_FALSE(fork_helper.WaitForChildren());
}

TEST_F(ProcessMemoryReleaseTest, NoPendingSIGKILL) {
  test_helper::ForkHelper fork_helper;
  pid_t child_pid = fork_helper.RunInForkedProcess([]() {
    // Wait to be killed by the parent.
    pause();
  });
  int child_pidfd = DoPidFdOpen(child_pid);
  ASSERT_THAT(child_pidfd, SyscallSucceeds());
  // Child process is still active when reaping.
  EXPECT_THAT(DoProcessMRelease(child_pidfd), SyscallFailsWithErrno(EINVAL));
  ASSERT_THAT(kill(child_pid, SIGKILL), SyscallSucceeds());
  ASSERT_FALSE(fork_helper.WaitForChildren());
}

TEST_F(ProcessMemoryReleaseTest, SuccessfulRelease) {
  // Create a pipe for synchronization between parent and child
  int pipe_fds[2];
  ASSERT_THAT(pipe(pipe_fds), SyscallSucceeds());
  int read_fd = pipe_fds[0];
  int write_fd = pipe_fds[1];

  // Using fork() instead of test_helper::ForkHelper to check details on the wait.
  pid_t child_pid = fork();

  if (child_pid == 0) {
    // Child process
    close(read_fd);
    // Allocate memory
    size_t page_size = sysconf(_SC_PAGE_SIZE);
    ASSERT_GT(page_size, static_cast<size_t>(0))
        << "sysconf(_SC_PAGE_SIZE) failed or returned invalid size";
    auto mem = test_helper::ScopedMMap::MMap(nullptr, kMemSizeBytes, PROT_READ | PROT_WRITE,
                                             MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
    if (mem.is_error()) {
      perror("child: mmap failed");
      close(write_fd);  // Ensure pipe is closed before exiting.
      _exit(EXIT_FAILURE);
    }

    // Touch memory to ensure pages are faulted in
    volatile char* p = static_cast<volatile char*>(mem->mapping());
    for (size_t i = 0; i < kMemSizeBytes; i += page_size) {
      p[i] = static_cast<char>(i % 256);
    }

    // Signal parent that memory allocation and initialization are done.
    char signal_byte = 'R';  // 'R' for Ready.
    ASSERT_THAT(write(write_fd, &signal_byte, 1), SyscallSucceedsWithValue(1));

    close(write_fd);

    // Wait to be killed by the parent.
    pause();

    // Clean up mmapped memory (though SIGKILL will typically prevent this).
    mem->Unmap();
    _exit(EXIT_SUCCESS);  // Should ideally not be reached if killed.
  }

  // Parent process
  close(write_fd);

  // Attach to the child with ptrace and wait for PTRACE_O_TRACEEXIT events.
  ASSERT_THAT(ptrace(PTRACE_SEIZE, child_pid, 0, PTRACE_O_TRACEEXIT), SyscallSucceeds())
      << "ptrace PTRACE_SEIZE failed: " << strerror(errno);

  // Wait for the child's signal indicating it's ready.
  char received_byte;
  ASSERT_THAT(read(read_fd, &received_byte, 1), SyscallSucceedsWithValue(1));
  ASSERT_EQ('R', received_byte) << "parent: received incorrect signal byte from child";
  close(read_fd);

  // Open a pidfd for the child process.
  int child_pidfd = DoPidFdOpen(child_pid);
  ASSERT_THAT(child_pidfd, SyscallSucceeds());
  ASSERT_THAT(kill(child_pid, SIGKILL), SyscallSucceeds());

  // Wait for the PTRACE_EVENT_EXIT
  int status;
  ASSERT_THAT(waitpid(child_pid, &status, 0), SyscallSucceedsWithValue(child_pid))
      << "waitpid for PTRACE_EVENT_EXIT failed: " << strerror(errno);
  ASSERT_TRUE(WIFSTOPPED(status)) << "Child not stopped for PTRACE_EVENT_EXIT";
  ASSERT_EQ(status >> 8, (SIGTRAP | (PTRACE_EVENT_EXIT << 8)))
      << "Child not stopped with PTRACE_EVENT_EXIT";

  // Verify the RSS size decreases 1MB and `maps` info has no change after memory is released.
  auto msize = GetProcessRSSMemorySize(child_pid);
  auto maps_str = GetMapsString(child_pid);
  EXPECT_THAT(DoProcessMRelease(child_pidfd), SyscallSucceeds());
  EXPECT_GT(msize, GetProcessRSSMemorySize(child_pid) + 1024);
  EXPECT_EQ(maps_str, GetMapsString(child_pid));

  // Wait for the child process to terminate and check its status.
  ASSERT_THAT(ptrace(PTRACE_CONT, child_pid, 0, 0), SyscallSucceeds());
  ASSERT_THAT(waitpid(child_pid, &status, 0), SyscallSucceeds());

  // Verify the child was terminated by SIGKILL.
  ASSERT_TRUE(WIFSIGNALED(status))
      << "parent: child process did not terminate due to a signal. Status: " << status;
  ASSERT_EQ(SIGKILL, WTERMSIG(status))
      << "parent: child process not terminated by SIGKILL. Terminating signal: "
      << WTERMSIG(status);
}
