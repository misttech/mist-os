// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fcntl.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <unistd.h>

#include <string>
#include <thread>

#include <gtest/gtest.h>
#include <linux/futex.h>

#include "src/lib/files/directory.h"
#include "src/lib/files/file.h"
#include "src/lib/files/path.h"
#include "src/starnix/tests/syscalls/test_helper.h"

namespace {

struct robust_list_entry {
  struct robust_list *next;
  int futex;
};

// Tests that robust_lists set futex FUTEX_OWNER_DIED bit if the thread that locked a futex
// dies without unlocking it.
TEST(RobustFutexTest, FutexStateCheck) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    robust_list_entry entry = {.next = nullptr, .futex = 0};
    robust_list_head head = {.list = {.next = nullptr},
                             .futex_offset = offsetof(robust_list_entry, futex),
                             .list_op_pending = nullptr};

    std::thread t([&entry, &head]() {
      head.list.next = reinterpret_cast<struct robust_list *>(&entry);
      EXPECT_EQ(0, syscall(SYS_set_robust_list, &head, sizeof(robust_list_head)));
      entry.futex = static_cast<int>(syscall(SYS_gettid));
      // Thread dies without releasing futex, so futex's FUTEX_OWNER_DIED bit is set.
    });
    t.join();
    EXPECT_EQ(FUTEX_OWNER_DIED, entry.futex & FUTEX_OWNER_DIED);
  });
}

// Tests that robust_lists set futex FUTEX_OWNER_DIED bit if the thread that locked a futex
// executes an exec() without unlocking it.
TEST(RobustFutexTest, FutexStateAfterExecCheck) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    // Allocate the futex and the robust list in shared memory.
    void *shared = mmap(nullptr, sizeof(robust_list_entry) + sizeof(robust_list_head),
                        PROT_READ | PROT_WRITE, MAP_SHARED | MAP_ANONYMOUS, -1, 0);
    EXPECT_NE(MAP_FAILED, shared) << "Error " << errno;

    robust_list_head *head = reinterpret_cast<robust_list_head *>(shared);
    robust_list_entry *entry = reinterpret_cast<robust_list_entry *>(
        reinterpret_cast<intptr_t>(shared) + sizeof(robust_list_head));

    *entry = {.next = nullptr, .futex = 0};
    *head = {.list = {.next = reinterpret_cast<struct robust_list *>(entry)},
             .futex_offset = offsetof(robust_list_entry, futex),
             .list_op_pending = nullptr};

    // Create a pipe that the child can use to notify parent process when it is running.
    int pipefd[2];
    pipe(pipefd);

    // Create a file we can lock.  After it notifies us that it is running via
    // the pipe, the child will wait to terminate until we unlock the file.
    test_helper::ScopedTempFD terminate_child_fd;
    struct flock fl = {.l_type = F_WRLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 0};
    EXPECT_EQ(0, fcntl(terminate_child_fd.fd(), F_SETLK, &fl));

    test_helper::ForkHelper helper;

    helper.RunInForkedProcess([&entry, &head, &terminate_child_fd, &pipefd] {
      // Redirect stdout to one end of the pipe
      EXPECT_NE(-1, dup2(pipefd[1], STDOUT_FILENO));
      EXPECT_EQ(0, syscall(SYS_set_robust_list, head, sizeof(robust_list_head)));
      entry->futex = static_cast<int>(syscall(SYS_gettid));

      std::string test_binary = "/data/tests/syscall_test_exec_child";
      if (!files::IsFile(test_binary)) {
        // We're running on host
        char self_path[PATH_MAX];
        realpath("/proc/self/exe", self_path);

        test_binary =
            files::JoinPath(files::GetDirectoryName(self_path), "syscall_test_exec_child");
      }
      char *const argv[] = {const_cast<char *>(test_binary.c_str()),
                            const_cast<char *>(terminate_child_fd.name().c_str()), nullptr};

      // execv happens without releasing futex, so futex's FUTEX_OWNER_DIED bit is set.
      execv(test_binary.c_str(), argv);
    });

    char buf[5];
    // Wait until the child process has performed the exec
    EXPECT_NE(-1, read(pipefd[0], reinterpret_cast<void *>(buf), 5));

    EXPECT_EQ(FUTEX_OWNER_DIED, entry->futex & FUTEX_OWNER_DIED);

    // Unlock the file, allowing the child process to continue (and exit).
    struct flock fl2 = {.l_type = F_UNLCK, .l_whence = SEEK_SET, .l_start = 0, .l_len = 0};
    EXPECT_EQ(0, fcntl(terminate_child_fd.fd(), F_SETLK, &fl2));
    EXPECT_EQ(true, helper.WaitForChildren());
    munmap(shared, sizeof(robust_list_entry) + sizeof(robust_list_head));
  });
}

}  // anonymous namespace
