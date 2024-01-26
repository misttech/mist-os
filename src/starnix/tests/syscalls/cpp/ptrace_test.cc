// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <elf.h>
#include <sched.h>
#include <signal.h>
#include <stdio.h>
#include <string.h>
#include <sys/ptrace.h>
#include <sys/uio.h>
#include <sys/user.h>
#include <syscall.h>
#include <time.h>
#include <unistd.h>

#include <linux/sched.h>

#ifdef __riscv
#include <asm/ptrace.h>
#endif  // __riscv

#include <gtest/gtest.h>

#include "src/lib/files/directory.h"
#include "src/lib/files/file.h"
#include "src/lib/files/path.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

constexpr int kOriginalSigno = SIGUSR1;
constexpr int kInjectedSigno = SIGUSR2;
constexpr int kInjectedErrno = EIO;

TEST(PtraceTest, SetSigInfo) {
  test_helper::ForkHelper helper;
  helper.OnlyWaitForForkedChildren();
  pid_t child_pid = helper.RunInForkedProcess([] {
    struct sigaction sa = {};
    sa.sa_sigaction = +[](int sig, siginfo_t *info, void *ucontext) {
      if (sig != kInjectedSigno) {
        _exit(1);
      }
      if (info->si_errno != kInjectedErrno) {
        _exit(2);
      }
      _exit(0);
    };

    sa.sa_flags = SA_SIGINFO | SA_RESTART;
    EXPECT_EQ(sigemptyset(&sa.sa_mask), 0);
    sigaction(kInjectedSigno, &sa, nullptr);
    sigaction(kOriginalSigno, &sa, nullptr);

    EXPECT_EQ(ptrace(PTRACE_TRACEME, 0, 0, 0), 0);
    raise(kOriginalSigno);
    _exit(3);
  });

  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == kOriginalSigno) << " status " << status;

  siginfo_t siginfo = {};
  ASSERT_EQ(ptrace(PTRACE_GETSIGINFO, child_pid, 0, &siginfo), 0)
      << "ptrace failed with error " << strerror(errno);
  EXPECT_EQ(kOriginalSigno, siginfo.si_signo);
  EXPECT_EQ(SI_TKILL, siginfo.si_code);

  // Replace the signal with kInjectedSigno, and check that the child exits
  // with kInjectedSigno, indicating that signal injection was successful.
  siginfo.si_signo = kInjectedSigno;
  siginfo.si_errno = kInjectedErrno;
  ASSERT_EQ(ptrace(PTRACE_SETSIGINFO, child_pid, 0, &siginfo), 0);
  ASSERT_EQ(ptrace(PTRACE_DETACH, child_pid, 0, kInjectedSigno), 0);
}

#ifndef PTRACE_EVENT_STOP  // Not defined in every libc
#define PTRACE_EVENT_STOP 128
#endif

TEST(PtraceTest, InterruptAfterListen) {
  volatile int child_should_spin = 1;
  test_helper::ForkHelper helper;
  helper.OnlyWaitForForkedChildren();
  pid_t child_pid = helper.RunInForkedProcess([&child_should_spin] {
    const struct timespec req = {.tv_sec = 0, .tv_nsec = 1000};
    while (child_should_spin) {
      nanosleep(&req, nullptr);
    }
    _exit(0);
  });

  // In parent process.
  ASSERT_NE(child_pid, 0);

  ASSERT_EQ(ptrace(PTRACE_SEIZE, child_pid, 0, 0), 0);
  int status;
  EXPECT_EQ(waitpid(child_pid, &status, WNOHANG), 0);

  // Stop the child with PTRACE_INTERRUPT.
  ASSERT_EQ(ptrace(PTRACE_INTERRUPT, child_pid, 0, 0), 0);
  ASSERT_EQ(waitpid(child_pid, &status, 0), child_pid);
  EXPECT_EQ(SIGTRAP | (PTRACE_EVENT_STOP << 8), status >> 8);

  ASSERT_EQ(ptrace(PTRACE_POKEDATA, child_pid, &child_should_spin, 0), 0) << strerror(errno);

  // Send SIGSTOP to the child, then resume it, allowing it to proceed to
  // signal-delivery-stop.
  ASSERT_EQ(kill(child_pid, SIGSTOP), 0);
  ASSERT_EQ(ptrace(PTRACE_CONT, child_pid, 0, 0), 0);
  ASSERT_EQ(waitpid(child_pid, &status, 0), child_pid);
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) << " status " << status;

  // Move out of signal-delivery-stop and deliver the SIGSTOP.
  ASSERT_EQ(ptrace(PTRACE_CONT, child_pid, 0, SIGSTOP), 0);
  ASSERT_EQ(waitpid(child_pid, &status, 0), child_pid);
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  EXPECT_EQ(SIGSTOP | (PTRACE_EVENT_STOP << 8), status >> 8);

  // Restart the child, but don't let it execute. Child continues to deliver
  // notifications of when it gets stop / continue signals.  This allows a
  // normal SIGCONT signal to be sent to a child to restart it, rather than
  // having the tracer restart it.  The tracer can then detect the SIGCONT.
  ASSERT_EQ(ptrace(PTRACE_LISTEN, child_pid, 0, 0), 0);

  // "If the tracee was already stopped by a signal and PTRACE_LISTEN was sent
  // to it, the tracee stops with PTRACE_EVENT_STOP and WSTOPSIG(status) returns
  // the stop signal."
  ASSERT_EQ(ptrace(PTRACE_INTERRUPT, child_pid, 0, 0), 0);
  ASSERT_EQ(waitpid(child_pid, &status, 0), child_pid);
  EXPECT_EQ(SIGSTOP | (PTRACE_EVENT_STOP << 8), status >> 8);

  // Allow the tracer to proceed normally.
  EXPECT_EQ(ptrace(PTRACE_CONT, child_pid, 0, 0), 0) << strerror(errno);
}

// None of this seems to be defined in our x64 and ARM sysroots.
#ifndef PTRACE_GET_SYSCALL_INFO
#define PTRACE_GET_SYSCALL_INFO 0x420e
#define PTRACE_SYSCALL_INFO_NONE 0
#define PTRACE_SYSCALL_INFO_ENTRY 1
#define PTRACE_SYSCALL_INFO_EXIT 2
#define PTRACE_SYSCALL_INFO_SECCOMP 3

struct ptrace_syscall_info {
  uint8_t op;
  uint8_t pad[3];
  uint32_t arch;
  uint64_t instruction_pointer;
  uint64_t stack_pointer;
  union {
    struct {
      uint64_t nr;
      uint64_t args[6];
    } entry;
    struct {
      int64_t rval;
      uint8_t is_error;
    } exit;
    struct {
      uint64_t nr;
      uint64_t args[6];
      uint32_t ret_data;
    } seccomp;
  };
};
#else
// In our RISC-V sysroot, this is called __ptrace_syscall_info
using ptrace_syscall_info = __ptrace_syscall_info;
#endif

TEST(PtraceTest, TraceSyscall) {
  test_helper::ForkHelper helper;
  helper.OnlyWaitForForkedChildren();
  pid_t child_pid = helper.RunInForkedProcess([] {
    EXPECT_EQ(ptrace(PTRACE_TRACEME, 0, 0, 0), 0);
    raise(SIGSTOP);
    struct timespec req = {.tv_sec = 0, .tv_nsec = 0};
    nanosleep(&req, nullptr);
  });

  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) << " status " << status;
  EXPECT_EQ(0, ptrace(PTRACE_SETOPTIONS, child_pid, 0, PTRACE_O_TRACESYSGOOD))
      << "error " << strerror(errno);

  ptrace_syscall_info info;
  const int kExpectedNoneSize =
      reinterpret_cast<uint8_t *>(&info.entry) - reinterpret_cast<uint8_t *>(&info);
  const int kExpectedEntrySize =
      reinterpret_cast<uint8_t *>(&info.entry.args[6]) - reinterpret_cast<uint8_t *>(&info);
  const int kExpectedExitSize =
      reinterpret_cast<uint8_t *>(&info.exit.is_error + 1) - reinterpret_cast<uint8_t *>(&info);

  // We are not at a syscall entry
  EXPECT_EQ(ptrace(static_cast<enum __ptrace_request>(PTRACE_GET_SYSCALL_INFO), child_pid,
                   sizeof(ptrace_syscall_info), &info),
            kExpectedNoneSize);
  EXPECT_EQ(info.op, PTRACE_SYSCALL_INFO_NONE);

  bool found = false;
  // We want to make sure we hit the "nanosleep" syscall.  There can be various
  // "hidden" syscalls in the tracee, depending on the implementation of "raise"
  // and "nanosleep".  So, we just keep trying until we hit nanosleep or exit.
  for (int i = 0; i < 10; i++) {
    EXPECT_EQ(ptrace(PTRACE_SYSCALL, child_pid, 0, 0), 0);
    EXPECT_EQ(child_pid, waitpid(child_pid, &status, 0));
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != (SIGTRAP | 0x80)) {
      break;
    }

    // We are now at a syscall entry
    EXPECT_EQ(ptrace(static_cast<enum __ptrace_request>(PTRACE_GET_SYSCALL_INFO), child_pid,
                     sizeof(ptrace_syscall_info), &info),
              kExpectedEntrySize);

    EXPECT_EQ(info.op, PTRACE_SYSCALL_INFO_ENTRY);
    switch (info.entry.nr) {
      case __NR_clock_nanosleep:
      case __NR_nanosleep:
        found = true;
        break;
      case __NR_exit:
      case __NR_exit_group:
        goto exit_loop;
    }

    EXPECT_EQ(ptrace(PTRACE_SYSCALL, child_pid, 0, 0), 0);
    EXPECT_EQ(child_pid, waitpid(child_pid, &status, 0));
    EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == (SIGTRAP | 0x80))
        << "WIFSTOPPED(status) " << WIFSTOPPED(status) << " WSTOPSIG(status) " << WSTOPSIG(status);

    // We are now at a syscall exit
    EXPECT_EQ(ptrace(static_cast<enum __ptrace_request>(PTRACE_GET_SYSCALL_INFO), child_pid,
                     sizeof(ptrace_syscall_info), &info),
              kExpectedExitSize);

    EXPECT_EQ(info.op, PTRACE_SYSCALL_INFO_EXIT);
    EXPECT_EQ(info.exit.rval, 0);
    EXPECT_EQ(info.exit.is_error, 0);
  }
exit_loop:

  EXPECT_EQ(found, true) << "Never found nanosleep call";
  EXPECT_EQ(ptrace(PTRACE_CONT, child_pid, 0, 0), 0);
}

TEST(PtraceTest, GetGeneralRegs) {
  test_helper::ForkHelper helper;
  helper.OnlyWaitForForkedChildren();
  pid_t child_pid = helper.RunInForkedProcess([] {
    EXPECT_EQ(ptrace(PTRACE_TRACEME, 0, 0, 0), 0);

    // Use kill explicitly because we check the syscall argument register below.
    kill(getpid(), SIGSTOP);

    _exit(0);
  });
  ASSERT_NE(child_pid, 0);

  // Wait for the child to send itself SIGSTOP and enter signal-delivery-stop.
  int status;
  ASSERT_EQ(waitpid(child_pid, &status, 0), child_pid);
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) << " status " << status;

#if defined(__x86_64__)
#define __REG rsi
#elif defined(__aarch64__)
#define __REG regs[1]
#elif defined(__riscv)
#define __REG a1
#else
#error "Test does not support architecture for PTRACE_GETREGS";
#endif

  // Get the general registers with PTRACE_GETREGSET
  struct user_regs_struct regs_set;
  struct iovec iov;
  iov.iov_base = &regs_set;
  iov.iov_len = sizeof(regs_set);
  EXPECT_EQ(ptrace(PTRACE_GETREGSET, child_pid, NT_PRSTATUS, &iov), 0)
      << "Error " << errno << " " << strerror(errno);

  // Read exactly the full register set.
  EXPECT_EQ(iov.iov_len, sizeof(regs_set));

  // Child called kill(2), with SIGSTOP as arg 2.
  EXPECT_EQ(regs_set.__REG, static_cast<unsigned long>(SIGSTOP));

  // The appropriate defines for this are not in the ptrace header for arm64.
#ifdef __x86_64__
  // Get the general registers, with PTRACE_GETREGS
  struct user_regs_struct regs_old;
  EXPECT_EQ(ptrace(PTRACE_GETREGS, child_pid, nullptr, &regs_old), 0)
      << "Error " << errno << " " << strerror(errno);

  EXPECT_EQ(regs_old.__REG, static_cast<unsigned long>(SIGSTOP));
#endif

  // Get the appropriate general register with PTRACE_PEEKUSER
  EXPECT_EQ(ptrace(PTRACE_PEEKUSER, child_pid, offsetof(struct user_regs_struct, __REG), nullptr),
            SIGSTOP)
      << "Error " << errno << " " << strerror(errno);

  // Suppress SIGSTOP and resume the child.
  ASSERT_EQ(ptrace(PTRACE_CONT, child_pid, 0, 0), 0);
  ASSERT_EQ(waitpid(child_pid, &status, 0), child_pid);

  // Let's see that process exited normally.
  EXPECT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0) << " status " << status;
}

namespace {
// As of this writing, our sysroot's syscall.h lacks the SYS_clone3 definition.
#ifndef SYS_clone3
#if defined(__aarch64__) || defined(__x86_64__) || defined(__riscv)
constexpr int SYS_clone3 = 435;
#else
#error SYS_clone3 needs a definition for this architecture.
#endif
#endif

// Generate a child process that will spawn a grandchild process,both of which
// will be traced.  We use SYS_clone3 directly here, as it removes libc
// discretion about whether this is fork/clone/vfork.
pid_t ForkUsingClone3(bool is_seized, uint64_t addl_clone_args) {
  struct clone_args ca;
  memset(&ca, 0, sizeof(ca));

  ca.flags = addl_clone_args;
  ca.exit_signal = SIGCHLD;  // Needed in order to wait on the child.

  pid_t child_pid = static_cast<pid_t>(syscall(SYS_clone3, &ca, sizeof(ca)));
  if (child_pid == 0) {
    if (!is_seized) {
      EXPECT_EQ(ptrace(PTRACE_TRACEME, 0, 0, 0), 0);
    }
    raise(SIGSTOP);
    pid_t grandchild_pid = static_cast<pid_t>(syscall(SYS_clone3, &ca, sizeof(ca)));
    if (grandchild_pid == 0) {
      // Automatically does a SIGSTOP if started traced
      exit(0);
    }
    int status;
    EXPECT_EQ(grandchild_pid, waitpid(grandchild_pid, &status, 0));
    EXPECT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0)
        << "Failure: WIFEXITED(status) =" << WIFEXITED(status)
        << " WEXITSTATUS(status) == " << WEXITSTATUS(status);
    exit(0);
  }
  EXPECT_GT(child_pid, 0) << strerror(errno);
  return child_pid;
}

void DetectForkAndContinue(pid_t child_pid, bool is_seized, bool child_stops_on_clone) {
  int status;
  pid_t grandchild_pid = 0;
  EXPECT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, 0));
  if (child_stops_on_clone) {
    // Continue until we hit a fork.
    ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));

    EXPECT_TRUE(WIFSTOPPED(status) && (status >> 8) == (SIGTRAP | (PTRACE_EVENT_FORK << 8)))
        << "status = " << status;

    // Get the grandchild's pid as reported by ptrace
    EXPECT_EQ(0, ptrace(PTRACE_GETEVENTMSG, child_pid, 0, &grandchild_pid)) << strerror(errno);
    EXPECT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, 0));
    // A grandchild started with TRACEFORK will start with a SIGSTOP or a PTRACE_EVENT_STOP
    // (depending on whether we used PTRACE_SEIZE to attach).
    EXPECT_EQ(grandchild_pid, waitpid(grandchild_pid, &status, 0)) << strerror(errno);
  } else {
    grandchild_pid = waitpid(0, &status, 0);
    EXPECT_NE(0, grandchild_pid) << strerror(errno);
  }

  if (is_seized) {
    EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP)
        << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
        << " WSTOPSIG = " << WSTOPSIG(status);
    int shifted_status = status >> 8;
    EXPECT_TRUE(((PTRACE_EVENT_STOP << 8) | SIGTRAP) == shifted_status)
        << "shifted_status = " << shifted_status;
  } else {
    EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) << " status " << status;
  }

  EXPECT_EQ(0, ptrace(PTRACE_CONT, grandchild_pid, 0, SIGCONT));

  // The grandchild should now exit.
  EXPECT_EQ(grandchild_pid, waitpid(grandchild_pid, &status, 0)) << strerror(errno);
  EXPECT_TRUE(WIFEXITED(status)) << "WIFEXITED(status) = " << WIFEXITED(status);

  // When the grandchild exits, the child receives a SIGCHLD.
  EXPECT_EQ(child_pid, waitpid(child_pid, &status, 0));
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGCHLD);
  EXPECT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, SIGCHLD));

  // The child should now exit
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0)
      << "WIFEXITED(status) == " << WIFEXITED(status)
      << " WEXITSTATUS(status) == " << WEXITSTATUS(status);
}
}  // namespace

TEST(PtraceTest, PtraceEventStopWithFork) {
  pid_t child_pid = ForkUsingClone3(false, 0);
  if (HasFatalFailure()) {
    return;
  }

  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) << " status " << status;
  EXPECT_EQ(0, ptrace(PTRACE_SETOPTIONS, child_pid, 0, PTRACE_O_TRACEFORK))
      << "error " << strerror(errno);

  DetectForkAndContinue(child_pid, false, true);
}

TEST(PtraceTest, PtraceEventStopWithForkAndSeize) {
  pid_t child_pid = ForkUsingClone3(true, 0);
  if (HasFatalFailure()) {
    return;
  }

  EXPECT_EQ(ptrace(PTRACE_SEIZE, child_pid, 0, PTRACE_O_TRACEFORK), 0) << strerror(errno);
  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) << " status " << status;

  DetectForkAndContinue(child_pid, true, true);
}

#if 0
// To be fixed
TEST(PtraceTest, PtraceEventStopWithForkClonePtrace) {
  pid_t child_pid = ForkUsingClone3(false, CLONE_PTRACE);
  if (HasFatalFailure()) {
    return;
  }
  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) << " status " << status;

  DetectForkAndContinue(child_pid, false, false);
}
#endif

constexpr int kBadExitStatus = 0xabababab;

pid_t DoExec() {
  pid_t child_pid = fork();
  if (child_pid == 0) {
    EXPECT_EQ(ptrace(PTRACE_TRACEME, 0, 0, 0), 0);
    raise(SIGSTOP);

    std::string test_binary = "/data/tests/ptrace_test_exec_child";
    if (!files::IsFile(test_binary)) {
      // We're running on host
      char self_path[PATH_MAX];
      realpath("/proc/self/exe", self_path);

      test_binary = files::JoinPath(files::GetDirectoryName(self_path), "ptrace_test_exec_child");
    }
    char *const argv[] = {const_cast<char *>(test_binary.c_str()), nullptr};

    // execv happens without releasing futex, so futex's FUTEX_OWNER_DIED bit is set.
    execve(test_binary.c_str(), argv, nullptr);
    // Should not get here.
    _exit(kBadExitStatus);
  }
  return child_pid;
}

// Ensure that the tracee sends a SIGTRAP when it encounters an exec and
// TRACEEXEC is not enabled.
TEST(PtraceTest, ExecveWithSigtrap) {
  pid_t child_pid = DoExec();

  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  EXPECT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, 0));

  EXPECT_EQ(child_pid, waitpid(child_pid, &status, 0));
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  EXPECT_EQ(0, ptrace(PTRACE_DETACH, child_pid, 0, 0));
  EXPECT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0)
      << "WIFEXITED(status) == " << WIFEXITED(status)
      << " WEXITSTATUS(status) == " << WEXITSTATUS(status);
}

// Ensure that, if TRACEEXIT is enabled, and the tracee executes an exit, it
// then sends a SIGTRAP | (PTRACE_EVENT_EXIT << 8)
TEST(PtraceTest, PtraceEventStopWithExit) {
  // TODO(https://fxbug.dev/322238868): This test does not work on the LTO
  // builder in CQ.
  if (!test_helper::IsStarnix()) {
    GTEST_SKIP() << "This test does not work on Linux in CQ";
  }

  pid_t child_pid = DoExec();

  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  EXPECT_EQ(0, ptrace(PTRACE_SETOPTIONS, child_pid, 0, PTRACE_O_TRACEEXIT))
      << "error " << strerror(errno);
  EXPECT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, 0));

  // Wait for the exec
  EXPECT_EQ(child_pid, waitpid(child_pid, &status, 0));
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);
  EXPECT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, 0));

  // Wait for the exit
  EXPECT_EQ(child_pid, waitpid(child_pid, &status, 0));
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  EXPECT_EQ(SIGTRAP | (PTRACE_EVENT_EXIT << 8), status >> 8);
  int exit_status = kBadExitStatus;
  EXPECT_EQ(ptrace(PTRACE_GETEVENTMSG, child_pid, 0, &exit_status), 0);
  // The actual exit status seems to change depending on how this test is run,
  // so just make sure that something is returned.
  EXPECT_TRUE(kBadExitStatus != exit_status)
      << "expected = " << kBadExitStatus << " actual: " << exit_status;
  EXPECT_EQ(0, ptrace(PTRACE_DETACH, child_pid, 0, 0));
  EXPECT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0)
      << "WIFEXITED(status) == " << WIFEXITED(status)
      << " WEXITSTATUS(status) == " << WEXITSTATUS(status);
}

// Ensure that, if TRACEEXEC is enabled, and the tracee executes an exec, it
// then sends a SIGTRAP | (PTRACE_EVENT_EXEC << 8).
TEST(PtraceTest, PtraceEventStopWithExecve) {
  // TODO(https://fxbug.dev/322238868): This test does not work on the LTO
  // builder in CQ.
  if (!test_helper::IsStarnix()) {
    GTEST_SKIP() << "This test does not work on Linux in CQ";
  }
  pid_t child_pid = DoExec();

  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  EXPECT_EQ(0, ptrace(PTRACE_SETOPTIONS, child_pid, 0, PTRACE_O_TRACEEXEC | PTRACE_O_TRACEEXIT))
      << "error " << strerror(errno);
  EXPECT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, 0));

  // Wait for the exec
  EXPECT_EQ(child_pid, waitpid(child_pid, &status, 0));
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  EXPECT_EQ(SIGTRAP | (PTRACE_EVENT_EXEC << 8), status >> 8);
  pid_t target_pid;
  EXPECT_EQ(ptrace(PTRACE_GETEVENTMSG, child_pid, 0, &target_pid), 0);
  EXPECT_EQ(target_pid, child_pid);

  EXPECT_EQ(0, ptrace(PTRACE_DETACH, child_pid, 0, 0));
  EXPECT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0)
      << "WIFEXITED(status) == " << WIFEXITED(status)
      << " WEXITSTATUS(status) == " << WEXITSTATUS(status);
}

// Ensure that, if TRACEEXIT is enabled, and the tracee is killed with a
// SIGTERM, it sends a SIGTRAP | (PTRACE_EVENT_EXIT << 8)
TEST(PtraceTest, PtraceEventStopWithSignalExit) {
  // TODO(https://fxbug.dev/322238868): This test does not work on the LTO
  // builder in CQ.
  if (!test_helper::IsStarnix()) {
    GTEST_SKIP() << "This test does not work on Linux in CQ";
  }

  pid_t child_pid = DoExec();

  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  EXPECT_EQ(0, ptrace(PTRACE_SETOPTIONS, child_pid, 0, PTRACE_O_TRACEEXIT))
      << "error " << strerror(errno);
  ASSERT_EQ(0, kill(child_pid, SIGTERM));
  EXPECT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, 0));

  // Wait for the signal-delivery-stop
  EXPECT_EQ(child_pid, waitpid(child_pid, &status, 0));
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGTERM)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);
  EXPECT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, SIGTERM));

  // Wait for the exit
  EXPECT_EQ(child_pid, waitpid(child_pid, &status, 0));
  EXPECT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  EXPECT_EQ(SIGTRAP | (PTRACE_EVENT_EXIT << 8), status >> 8);
  int exit_status = 0xabababab;
  EXPECT_EQ(ptrace(PTRACE_GETEVENTMSG, child_pid, 0, &exit_status), 0);
  EXPECT_TRUE(SIGTERM == exit_status) << " exit_status " << exit_status;
  EXPECT_EQ(0, ptrace(PTRACE_DETACH, child_pid, 0, 0));
  EXPECT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSIGNALED(status) && WTERMSIG(status) == SIGTERM)
      << "WIFSIGNALED(status) == " << WIFEXITED(status)
      << " WTERMSIG(status) == " << WTERMSIG(status);
}
