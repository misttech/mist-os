// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <signal.h>
#include <sys/prctl.h>
#include <sys/syscall.h>

#include <thread>

#include <gtest/gtest.h>
#include <linux/filter.h>
#include <linux/seccomp.h>

namespace {

// A syscall not implemented by Linux that we don't expect to be called.
#ifdef __x86_64__
constexpr uint32_t kFilteredSyscall = SYS_vserver;
#elif __aarch64__
// Use the last of arch_specific_syscalls which are not implemented on arm64.
constexpr uint32_t kFilteredSyscall = __NR_arch_specific_syscall + 15;
#endif

void exit_with_failure_code() {
  if (testing::Test::HasFatalFailure() || testing::Test::HasNonfatalFailure()) {
    exit(1);
  } else {
    exit(0);
  }
}
// Installs a filter that blocks the given syscall.
void install_filter_block(uint32_t syscall_nr, uint32_t action) {
  EXPECT_GE(0, prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0));

  sock_filter filter[] = {
      // A = seccomp_data.nr
      BPF_STMT(BPF_LD | BPF_ABS | BPF_W, offsetof(struct seccomp_data, nr)),
      // if (A == sysno) goto kill
      BPF_JUMP(BPF_JMP | BPF_JEQ | BPF_K, syscall_nr, 1, 0),
      // allow: return SECCOMP_RET_ALLOW
      BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
      // kill: return SECCOMP_RET_KILL_PROCESS
      BPF_STMT(BPF_RET | BPF_K, action),
  };
  sock_fprog prog = {.len = sizeof(filter) / sizeof(sock_filter), .filter = filter};
  EXPECT_EQ(0, syscall(__NR_seccomp, SECCOMP_SET_MODE_FILTER, 0, &prog))
      << "syscall failed with errno " << errno;
}

TEST(SeccompTest, RetTrapBypassesIgn) {
  pid_t const pid = fork();
  if (pid == 0) {
    std::thread t([]() {
      install_filter_block(kFilteredSyscall, SECCOMP_RET_TRAP);

      signal(SIGSYS, SIG_IGN);

      syscall(kFilteredSyscall);
      exit_with_failure_code();
    });
    // Should never join - process should be killed.
    t.join();
  };
  ASSERT_NE(pid, -1) << "Fork failed";
  int status;
  ASSERT_NE(waitpid(pid, &status, 0), -1);
  EXPECT_TRUE(WIFSIGNALED(status) && WTERMSIG(status) == SIGSYS) << "status " << status;
}

}  // anonymous namespace
