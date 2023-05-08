// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <signal.h>
#include <sys/prctl.h>
#include <sys/syscall.h>

#include <array>
#include <thread>

#include <gtest/gtest.h>
#include <linux/capability.h>
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

#define ARRAY_SIZE(x) (sizeof((x)) / sizeof((x[0])))

bool HasSysAdmin() {
  struct __user_cap_header_struct header = {_LINUX_CAPABILITY_VERSION_3, 0};
  struct __user_cap_data_struct caps[_LINUX_CAPABILITY_U32S_3] = {};
  syscall(__NR_capget, &header, &caps);

  return (caps[CAP_TO_INDEX(CAP_SYS_ADMIN)].effective & CAP_TO_MASK(CAP_SYS_ADMIN)) != 0;
}

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
  sock_fprog prog = {.len = ARRAY_SIZE(filter), .filter = filter};
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

// Test to ensure strict mode works.
TEST(SeccompTest, Strict) {
  pid_t const pid = fork();
  if (pid == 0) {
    EXPECT_GE(0, prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0));
    EXPECT_EQ(0, prctl(PR_SET_SECCOMP, SECCOMP_MODE_STRICT, 0, 0, 0));
    syscall(kFilteredSyscall);
  };
  ASSERT_NE(pid, -1) << "Fork failed";
  int status;
  ASSERT_NE(waitpid(pid, &status, 0), -1);
  EXPECT_TRUE(WIFSIGNALED(status) && WTERMSIG(status) == SIGKILL) << "status " << status;
}

// Cannot change from Filtered to Strict
TEST(SeccompTest, FilterToStrictErrors) {
  pid_t const pid = fork();
  if (pid == 0) {
    ASSERT_GE(0, prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0));

    sock_filter filter[] = {
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    sock_fprog prog = {.len = sizeof(filter) / sizeof(sock_filter), .filter = filter};
    EXPECT_EQ(0, syscall(__NR_seccomp, SECCOMP_SET_MODE_FILTER, 0, &prog));

    EXPECT_EQ(-1, prctl(PR_SET_SECCOMP, SECCOMP_MODE_STRICT, 0, 0, 0))
        << "Was able to set a seccomp filter to strict when there was a filter " << errno;

    EXPECT_EQ(EINVAL, errno);
    exit_with_failure_code();
  };
  ASSERT_NE(pid, -1) << "Fork failed";
  int status;
  ASSERT_NE(waitpid(pid, &status, 0), -1);
  EXPECT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0) << "status " << status;
}

// Checks for attempt to install null filter with FILTER, or non-null filter with STRICT
TEST(SeccompTest, BadArgs) {
  pid_t const pid = fork();
  if (pid == 0) {
    EXPECT_GE(0, prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0));
    EXPECT_EQ(-1, prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, NULL, NULL, NULL));
    EXPECT_EQ(EFAULT, errno);

    sock_filter filter[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, offsetof(struct seccomp_data, nr)),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    sock_fprog prog = {
        .len = ARRAY_SIZE(filter),
        .filter = filter,
    };

    EXPECT_EQ(-1, prctl(PR_SET_SECCOMP, SECCOMP_MODE_STRICT, &prog, NULL, NULL));
    EXPECT_EQ(EINVAL, errno);

    exit_with_failure_code();
  }
  ASSERT_NE(pid, -1) << "Fork failed";
  int status;
  ASSERT_NE(waitpid(pid, &status, 0), -1);
  EXPECT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0) << "status " << status;
}

// Test to attempt to install filter without the right caps.
TEST(SeccompTest, BadNoNewPrivs) {
  // Skip this test if we are root.
  if (HasSysAdmin()) {
    GTEST_SKIP() << "Skipped perms test because running as root";
  }
  pid_t const pid = fork();
  if (pid == 0) {
    // Neither CAP_SYS_ADMIN nor NO_NEW_PRIVS == 1
    EXPECT_EQ(-1, prctl(PR_SET_SECCOMP, SECCOMP_MODE_STRICT, 0, 0, 0));
    EXPECT_EQ(EACCES, errno);
    exit_with_failure_code();
  }
  ASSERT_NE(pid, -1) << "Fork failed";
  int status;
  ASSERT_NE(waitpid(pid, &status, 0), -1);
  ASSERT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0) << "status " << status;
}

TEST(SeccompTest, SeccompMax4KMinOne) {
  pid_t const pid = fork();
  if (pid == 0) {
    EXPECT_GE(0, prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0));
    constexpr uint64_t too_big = BPF_MAXINSNS + 1;
    auto filter = std::make_unique<sock_filter[]>(too_big);
    sock_filter val = BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW);
    std::fill(filter.get(), filter.get() + too_big, val);

    EXPECT_GE(0, prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)) << "prctl failed";

    struct sock_fprog fprog = {.len = too_big, .filter = filter.get()};

    EXPECT_NE(0, prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &fprog, 0, 0))
        << "Too large a filter was allowed";

    sock_filter zero_filter[] = {};
    sock_fprog zero_prog = {
        .len = 0,
        .filter = zero_filter,
    };
    EXPECT_NE(0, prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &zero_prog, 0, 0))
        << "Zero size filter was allowed";

    exit_with_failure_code();
  };
  ASSERT_NE(pid, -1) << "Fork failed";
  int status;
  ASSERT_NE(waitpid(pid, &status, 0), -1);
  ASSERT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0) << "status " << status;
}

#define MAX_INSNS_PER_PATH 32768

// Ensure that the total number of instructions in filters does not exceed the
// maximum.  Note that each filter has a fixed overhead of 4 instructions.
TEST(SeccompTest, SeccompMaxTotalInsns) {
  pid_t const pid = fork();
  if (pid == 0) {
    EXPECT_GE(0, prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0));
    auto filter = std::make_unique<sock_filter[]>(BPF_MAXINSNS);
    sock_filter val = BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW);
    std::fill(filter.get(), filter.get() + BPF_MAXINSNS, val);

    EXPECT_GE(0, prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0)) << "prctl failed";

    struct sock_fprog fprog = {.len = BPF_MAXINSNS, .filter = filter.get()};

    for (int i = 0; i < (MAX_INSNS_PER_PATH / BPF_MAXINSNS) - 1; i++) {
      EXPECT_EQ(0, prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &fprog, 0, 0));
    }

    EXPECT_EQ(-1, prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &fprog, 0, 0));
    EXPECT_EQ(errno, ENOMEM);

    exit_with_failure_code();
  };
  ASSERT_NE(pid, -1) << "Fork failed";
  int status;
  ASSERT_NE(waitpid(pid, &status, 0), -1);
  ASSERT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0) << "status " << status;
}

// If a BPF operation contains BPF_ABS, then the specified offset has to be
// aligned to a 32-bit boundary and not exceed sizeof(seccomp_data)
TEST(SeccompTest, FilterAccessInBounds) {
  pid_t const pid = fork();
  if (pid == 0) {
    EXPECT_GE(0, prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0));

    sock_filter filter[] = {
        BPF_STMT(BPF_LD | BPF_W | BPF_ABS, sizeof(seccomp_data) + 4),
        BPF_STMT(BPF_RET | BPF_K, SECCOMP_RET_ALLOW),
    };
    sock_fprog prog = {
        .len = ARRAY_SIZE(filter),
        .filter = filter,
    };
    EXPECT_EQ(-1, prctl(PR_SET_SECCOMP, SECCOMP_MODE_FILTER, &prog, 0, 0))
        << "unaligned abs was allowed in filter";

    EXPECT_EQ(EINVAL, errno) << "unaligned abs did not set set errno correctly";
    exit_with_failure_code();
  };
  ASSERT_NE(pid, -1) << "Fork failed";
  int status;
  ASSERT_NE(waitpid(pid, &status, 0), -1);
  ASSERT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0) << "status " << status;
}

TEST(SeccompTest, RetKillProcess) {
  pid_t const pid = fork();
  if (pid == 0) {
    std::thread t([]() {
      install_filter_block(kFilteredSyscall, SECCOMP_RET_KILL_PROCESS);
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

TEST(SeccompTest, KillProcessOnBadRetAction) {
  pid_t const pid = fork();
  if (pid == 0) {
    std::thread t([]() {
      install_filter_block(kFilteredSyscall, 0x00020000U);
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

TEST(SeccompTest, PrctlGetSeccomp) {
  pid_t const pid = fork();
  if (pid == 0) {
    std::thread t([]() {
      EXPECT_GE(0, prctl(PR_SET_NO_NEW_PRIVS, 1, 0, 0, 0));

      // prctl(PR_GET_SECCOMP) with no filter returns 0
      EXPECT_EQ(0, prctl(PR_GET_SECCOMP, 0, 0, 0, 0));

      install_filter_block(kFilteredSyscall, SECCOMP_RET_KILL_PROCESS);

      // prctl(PR_GET_SECCOMP) with a filter that does not block prctl returns 2
      EXPECT_EQ(2, prctl(PR_GET_SECCOMP, 0, 0, 0, 0));

      install_filter_block(__NR_prctl, SECCOMP_RET_KILL_PROCESS);

      // We're about to kill the process, so if there weer any failures before this,
      // report them and exit.
      if (testing::Test::HasFatalFailure() || testing::Test::HasNonfatalFailure()) {
        exit(1);
      }

      // prctl(PR_GET_SECCOMP) with a filter that blocks prctl kills the process
      prctl(PR_GET_SECCOMP, 0, 0, 0, 0);
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
