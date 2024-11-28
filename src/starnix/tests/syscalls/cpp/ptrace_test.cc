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
    ASSERT_EQ(sigemptyset(&sa.sa_mask), 0);
    sigaction(kInjectedSigno, &sa, nullptr);
    sigaction(kOriginalSigno, &sa, nullptr);

    ASSERT_EQ(ptrace(PTRACE_TRACEME, 0, 0, 0), 0);
    raise(kOriginalSigno);
    _exit(3);
  });

  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == kOriginalSigno) << " status " << status;

  siginfo_t siginfo = {};
  ASSERT_EQ(ptrace(PTRACE_GETSIGINFO, child_pid, 0, &siginfo), 0)
      << "ptrace failed with error " << strerror(errno);
  ASSERT_EQ(kOriginalSigno, siginfo.si_signo);
  ASSERT_EQ(SI_TKILL, siginfo.si_code);

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
  ASSERT_EQ(waitpid(child_pid, &status, WNOHANG), 0);

  // Stop the child with PTRACE_INTERRUPT.
  ASSERT_EQ(ptrace(PTRACE_INTERRUPT, child_pid, 0, 0), 0);
  ASSERT_EQ(waitpid(child_pid, &status, 0), child_pid);
  ASSERT_EQ(SIGTRAP | (PTRACE_EVENT_STOP << 8), status >> 8);

  ASSERT_EQ(ptrace(PTRACE_POKEDATA, child_pid, &child_should_spin, 0), 0) << strerror(errno);

  // Send SIGSTOP to the child, then resume it, allowing it to proceed to
  // signal-delivery-stop.
  ASSERT_EQ(kill(child_pid, SIGSTOP), 0);
  ASSERT_EQ(ptrace(PTRACE_CONT, child_pid, 0, 0), 0);
  ASSERT_EQ(waitpid(child_pid, &status, 0), child_pid);
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) << " status " << status;

  // Move out of signal-delivery-stop and deliver the SIGSTOP.
  ASSERT_EQ(ptrace(PTRACE_CONT, child_pid, 0, SIGSTOP), 0);
  ASSERT_EQ(waitpid(child_pid, &status, 0), child_pid);
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  ASSERT_EQ(SIGSTOP | (PTRACE_EVENT_STOP << 8), status >> 8);

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
  ASSERT_EQ(SIGSTOP | (PTRACE_EVENT_STOP << 8), status >> 8);

  // Allow the tracer to proceed normally.
  ASSERT_EQ(ptrace(PTRACE_CONT, child_pid, 0, 0), 0) << strerror(errno);
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
    ASSERT_EQ(ptrace(PTRACE_TRACEME, 0, 0, 0), 0);
    raise(SIGSTOP);
    struct timespec req = {.tv_sec = 0, .tv_nsec = 0};
    nanosleep(&req, nullptr);
  });

  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) << " status " << status;
  ASSERT_EQ(0, ptrace(PTRACE_SETOPTIONS, child_pid, 0, PTRACE_O_TRACESYSGOOD))
      << "error " << strerror(errno);

  ptrace_syscall_info info;
  const int kExpectedNoneSize =
      reinterpret_cast<uint8_t *>(&info.entry) - reinterpret_cast<uint8_t *>(&info);
  const int kExpectedEntrySize =
      reinterpret_cast<uint8_t *>(&info.entry.args[6]) - reinterpret_cast<uint8_t *>(&info);
  const int kExpectedExitSize =
      reinterpret_cast<uint8_t *>(&info.exit.is_error + 1) - reinterpret_cast<uint8_t *>(&info);

  // We are not at a syscall entry
  ASSERT_EQ(ptrace(static_cast<enum __ptrace_request>(PTRACE_GET_SYSCALL_INFO), child_pid,
                   sizeof(ptrace_syscall_info), &info),
            kExpectedNoneSize);
  ASSERT_EQ(info.op, PTRACE_SYSCALL_INFO_NONE);

  bool found = false;
  // We want to make sure we hit the "nanosleep" syscall.  There can be various
  // "hidden" syscalls in the tracee, depending on the implementation of "raise"
  // and "nanosleep".  So, we just keep trying until we hit nanosleep or exit.
  for (int i = 0; i < 10; i++) {
    ASSERT_EQ(ptrace(PTRACE_SYSCALL, child_pid, 0, 0), 0);
    ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
    if (!WIFSTOPPED(status) || WSTOPSIG(status) != (SIGTRAP | 0x80)) {
      break;
    }

    // We are now at a syscall entry
    ASSERT_EQ(ptrace(static_cast<enum __ptrace_request>(PTRACE_GET_SYSCALL_INFO), child_pid,
                     sizeof(ptrace_syscall_info), &info),
              kExpectedEntrySize);

    ASSERT_EQ(info.op, PTRACE_SYSCALL_INFO_ENTRY);
    switch (info.entry.nr) {
      case __NR_clock_nanosleep:
      case __NR_nanosleep:
        found = true;
        break;
      case __NR_exit:
      case __NR_exit_group:
        goto exit_loop;
    }

    ASSERT_EQ(ptrace(PTRACE_SYSCALL, child_pid, 0, 0), 0);
    ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
    ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == (SIGTRAP | 0x80))
        << "WIFSTOPPED(status) " << WIFSTOPPED(status) << " WSTOPSIG(status) " << WSTOPSIG(status);

    // We are now at a syscall exit
    ASSERT_EQ(ptrace(static_cast<enum __ptrace_request>(PTRACE_GET_SYSCALL_INFO), child_pid,
                     sizeof(ptrace_syscall_info), &info),
              kExpectedExitSize);

    ASSERT_EQ(info.op, PTRACE_SYSCALL_INFO_EXIT);
    ASSERT_EQ(info.exit.rval, 0);
    ASSERT_EQ(info.exit.is_error, 0);
  }
exit_loop:

  ASSERT_EQ(found, true) << "Never found nanosleep call";
  ASSERT_EQ(ptrace(PTRACE_CONT, child_pid, 0, 0), 0);
}

#ifdef __x86_64__

static constexpr int kUnmaskedSignal = SIGUSR1;

// Linux has internal errnos that capture the circumstances when an interrupted
// syscall should restart rather than return.  These are ordinarily invisible to
// the user - the syscall is either restarted, or the internal errno is replaced
// by EINTR.  However, ptrace can detect them on ptrace-syscall-exit.
void TraceSyscallWithRestartWithCall(int call, long arg0, long arg1, long arg2, long arg3,
                                     int expected_errno) {
  test_helper::ForkHelper helper;
  helper.OnlyWaitForForkedChildren();
  helper.ExpectSignal(SIGKILL);
  pid_t child_pid = helper.RunInForkedProcess([call, arg0, arg1, arg2, arg3] {
    struct sigaction sa = {};
    sa.sa_handler = [](int signo) {};
    ASSERT_EQ(sigfillset(&sa.sa_mask), 0);
    ASSERT_EQ(sigaction(kUnmaskedSignal, &sa, nullptr), 0);
    ASSERT_EQ(sigprocmask(SIG_UNBLOCK, &sa.sa_mask, nullptr), 0);

    ASSERT_EQ(ptrace(PTRACE_TRACEME, 0, 0, 0), 0);
    raise(SIGSTOP);

    // When the following syscalls are interrupted, errno should be some weird
    // internal errno (expected_errno above).  This means that the syscall will
    // return -1 if it is interrupted by a signal that has a user handler.
    ASSERT_EQ(-1, syscall(call, arg0, arg1, arg2, arg3));
    ASSERT_EQ(EINTR, errno) << strerror(errno);
  });

  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  struct user_regs_struct regs = {};
  int count = 0;
  do {
    // Suppress the SIGSTOP and wait for the child to enter syscall-enter-stop
    // for the given syscall.  Repeat this in case we're using a libc where
    // raise() makes a syscall after sending the signal.
    ASSERT_EQ(ptrace(PTRACE_SYSCALL, child_pid, 0, 0), 0);
    ASSERT_EQ(waitpid(child_pid, &status, 0), child_pid);
    ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP) << " status " << status;

    ASSERT_EQ(ptrace(PTRACE_GETREGS, child_pid, 0, &regs), 0);
    count += 1;
  } while (static_cast<int>(regs.orig_rax) != call && count < 100);
  ASSERT_EQ(call, static_cast<int>(regs.orig_rax));
  ASSERT_EQ(-ENOSYS, static_cast<int>(regs.rax));

  // Resume the child with PTRACE_SYSCALL and expect it to block in the syscall.
  ASSERT_EQ(ptrace(PTRACE_SYSCALL, child_pid, 0, 0), 0);
  test_helper::WaitUntilBlocked(child_pid, true);
  ASSERT_EQ(waitpid(child_pid, &status, WNOHANG), 0);

  // Send the child kUnmaskedSignal, causing it to return the given errno and enter
  // syscall-exit-stop from the syscall.
  ASSERT_EQ(kill(child_pid, kUnmaskedSignal), 0);
  ASSERT_EQ(waitpid(child_pid, &status, 0), child_pid);
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP) << " status " << status;

  ASSERT_EQ(ptrace(PTRACE_GETREGS, child_pid, 0, &regs), 0);
  ASSERT_EQ(call, static_cast<int>(regs.orig_rax));
  ASSERT_EQ(-expected_errno, static_cast<int>(regs.rax));

  kill(child_pid, SIGKILL);
  ptrace(PTRACE_DETACH, child_pid, 0, 0);
}

static constexpr int ERESTARTNOHAND = 514;
static constexpr int ERESTART_RESTARTBLOCK = 516;

TEST(PtraceTest, TraceSyscallWithRestart_pause) {
  ASSERT_NO_FATAL_FAILURE(TraceSyscallWithRestartWithCall(SYS_pause, 0, 0, 0, 0, ERESTARTNOHAND));
}

TEST(PtraceTest, TraceSyscallWithRestart_nanosleep) {
  const struct timespec req = {.tv_sec = 10, .tv_nsec = 0};
  ASSERT_NO_FATAL_FAILURE(TraceSyscallWithRestartWithCall(
      SYS_nanosleep, reinterpret_cast<long>(&req), 0, 0, 0, ERESTART_RESTARTBLOCK));
}

TEST(PtraceTest, TraceSyscallWithRestart_rt_sigsuspend) {
  sigset_t sigset;
  ASSERT_EQ(0, sigfillset(&sigset));
  ASSERT_EQ(0, sigdelset(&sigset, kUnmaskedSignal));
  ASSERT_NO_FATAL_FAILURE(
      TraceSyscallWithRestartWithCall(SYS_rt_sigsuspend, reinterpret_cast<long>(&sigset),
                                      sizeof(unsigned long), 0, 0, ERESTARTNOHAND));
}

TEST(PtraceTest, TraceSyscallWithRestart_ppoll) {
  struct timespec req = {.tv_sec = 10, .tv_nsec = 0};
  ASSERT_NO_FATAL_FAILURE(TraceSyscallWithRestartWithCall(
      SYS_ppoll, 0, 0, reinterpret_cast<long>(&req), 0, ERESTARTNOHAND));
}

TEST(PtraceTest, PokeUser) {
  test_helper::ForkHelper helper;
  helper.OnlyWaitForForkedChildren();
  constexpr long kStartPattern = 0xabababab;
  constexpr long kEndPattern = 0xcdcdcdcd;

  pid_t child_pid = helper.RunInForkedProcess([kEndPattern] {
    ASSERT_EQ(ptrace(PTRACE_TRACEME, 0, 0, 0), 0);
    long output;

    asm volatile("movq %0, %%rdi"
                 :  // No output
                 : "r"(kStartPattern));
    // Use kill explicitly because we check the syscall argument register below.
    kill(getpid(), SIGSTOP);

    asm volatile("movq %%rdi, %0" : "=r"(output));
    ASSERT_EQ(output, kEndPattern);
  });

  ASSERT_NE(child_pid, 0);

  // Wait for the child to send itself SIGSTOP and enter signal-delivery-stop.
  int status;
  ASSERT_EQ(waitpid(child_pid, &status, 0), child_pid);
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) << " status " << status;

  ASSERT_EQ(0,
            ptrace(PTRACE_POKEUSER, child_pid, offsetof(struct user_regs_struct, rdi), kEndPattern))
      << strerror(errno);

  ASSERT_EQ(0, ptrace(PTRACE_DETACH, child_pid, 0, SIGCONT));
}

#endif  // __x86_64__

TEST(PtraceTest, GetGeneralRegs) {
  test_helper::ForkHelper helper;
  helper.OnlyWaitForForkedChildren();
  pid_t child_pid = helper.RunInForkedProcess([] {
    ASSERT_EQ(ptrace(PTRACE_TRACEME, 0, 0, 0), 0);

    // Use kill explicitly because we check the syscall argument register below.
    kill(getpid(), SIGSTOP);

    _exit(0);
  });
  ASSERT_NE(child_pid, 0);

  // Wait for the child to send itself SIGSTOP and enter signal-delivery-stop.
  int status;
  ASSERT_EQ(waitpid(child_pid, &status, 0), child_pid);
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) << " status " << status;

#if defined(__x86_64__)
#define __REG rsi
#elif defined(__aarch64__)
#define __REG regs[1]
#elif defined(__riscv)
#define __REG a1
#else
#error "Test does not support architecture for PTRACE_GETREGS";
#endif

  // Get the general registers with PTRACE_GETREGSET. Make this too large so
  // that ptrace can provide the correct value.
  struct user_regs_struct regs_set[2];
  struct iovec iov;
  iov.iov_base = regs_set;

  // Expect error on incorrect size.
  iov.iov_len = sizeof(regs_set[0]) - 1;
  ASSERT_EQ(ptrace(PTRACE_GETREGSET, child_pid, NT_PRSTATUS, &iov), -1)
      << "Error " << errno << " " << strerror(errno);
  ASSERT_EQ(errno, EINVAL);

  // Provide a too large value for iov_len to make sure that ptrace resets it
  // correctly
  iov.iov_len = sizeof(regs_set);
  ASSERT_EQ(ptrace(PTRACE_GETREGSET, child_pid, NT_PRSTATUS, &iov), 0)
      << "Error " << errno << " " << strerror(errno);

  // Make sure ptrace set the correct size for the user_regs_struct.
  ASSERT_EQ(iov.iov_len, sizeof(struct user_regs_struct));

  // Child called kill(2), with SIGSTOP as arg 2.
  ASSERT_EQ(regs_set[0].__REG, static_cast<unsigned long>(SIGSTOP));

  // The appropriate defines for this are not in the ptrace header for arm64.
#ifdef __x86_64__
  // Get the general registers, with PTRACE_GETREGS
  struct user_regs_struct regs_old;
  ASSERT_EQ(ptrace(PTRACE_GETREGS, child_pid, nullptr, &regs_old), 0)
      << "Error " << errno << " " << strerror(errno);

  ASSERT_EQ(regs_old.__REG, static_cast<unsigned long>(SIGSTOP));
#endif

  // Get the appropriate general register with PTRACE_PEEKUSER
  ASSERT_EQ(ptrace(PTRACE_PEEKUSER, child_pid, offsetof(struct user_regs_struct, __REG), nullptr),
            SIGSTOP)
      << "Error " << errno << " " << strerror(errno);

  // Suppress SIGSTOP and resume the child.
  ASSERT_EQ(ptrace(PTRACE_DETACH, child_pid, 0, 0), 0);
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
void ForkUsingClone3(bool is_seized, uint64_t addl_clone_args, pid_t *out) {
  struct clone_args ca;
  memset(&ca, 0, sizeof(ca));

  ca.flags = addl_clone_args;
  ca.exit_signal = SIGCHLD;  // Needed in order to wait on the child.

  pid_t child_pid = static_cast<pid_t>(syscall(SYS_clone3, &ca, sizeof(ca)));
  if (child_pid == 0) {
    if (!is_seized) {
      ASSERT_EQ(ptrace(PTRACE_TRACEME, 0, 0, 0), 0);
    }
    raise(SIGSTOP);
    pid_t grandchild_pid = static_cast<pid_t>(syscall(SYS_clone3, &ca, sizeof(ca)));
    if (grandchild_pid == 0) {
      // Automatically does a SIGSTOP if started traced
      exit(0);
    }
    int status;
    ASSERT_EQ(grandchild_pid, waitpid(grandchild_pid, &status, 0)) << strerror(errno);
    ASSERT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0)
        << "Failure: WIFEXITED(status) =" << WIFEXITED(status)
        << " WEXITSTATUS(status) == " << WEXITSTATUS(status);
    exit(0);
  }
  ASSERT_GT(child_pid, 0) << strerror(errno);
  *out = child_pid;
}

template <typename T>
long get_event_msg(pid_t traced_pid, T *message) {
  unsigned long value;
  long return_code = ptrace(PTRACE_GETEVENTMSG, traced_pid, 0, &value);
  *message = static_cast<T>(value);
  return return_code;
}

void DetectForkAndContinue(pid_t child_pid, bool is_seized, bool child_stops_on_clone) {
  int status;
  pid_t grandchild_pid = 0;
  ASSERT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, 0));
  if (child_stops_on_clone) {
    // Continue until we hit a fork.
    ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));

    ASSERT_TRUE(WIFSTOPPED(status) && (status >> 8) == (SIGTRAP | (PTRACE_EVENT_FORK << 8)))
        << "status = " << status;

    // Get the grandchild's pid as reported by ptrace
    ASSERT_EQ(0, get_event_msg<pid_t>(child_pid, &grandchild_pid))
        << strerror(errno) << ": with child pid: " << child_pid;
    ASSERT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, 0))
        << strerror(errno) << " with child pid " << child_pid;
    // A grandchild started with TRACEFORK will start with a SIGSTOP or a PTRACE_EVENT_STOP
    // (depending on whether we used PTRACE_SEIZE to attach).
    ASSERT_EQ(grandchild_pid, waitpid(grandchild_pid, &status, 0)) << strerror(errno);
  } else {
    grandchild_pid = waitpid(0, &status, 0);
    ASSERT_NE(-1, grandchild_pid) << strerror(errno);
  }

  if (is_seized) {
    ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP)
        << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
        << " WSTOPSIG = " << WSTOPSIG(status);
    int shifted_status = status >> 8;
    ASSERT_TRUE(((PTRACE_EVENT_STOP << 8) | SIGTRAP) == shifted_status)
        << "shifted_status = " << shifted_status;
  } else {
    ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP)
        << " status " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
        << " WSTOPSIG = " << WSTOPSIG(status);
  }

  ASSERT_EQ(0, ptrace(PTRACE_CONT, grandchild_pid, 0, SIGCONT));

  // The grandchild should now exit.
  ASSERT_EQ(grandchild_pid, waitpid(grandchild_pid, &status, 0)) << strerror(errno);
  ASSERT_TRUE(WIFEXITED(status)) << "WIFEXITED(status) = " << WIFEXITED(status);

  // When the grandchild exits, the child receives a SIGCHLD.
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGCHLD);
  ASSERT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, SIGCHLD));

  // The child should now exit
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0)
      << "WIFEXITED(status) == " << WIFEXITED(status)
      << " WEXITSTATUS(status) == " << WEXITSTATUS(status);
}
}  // namespace

TEST(PtraceTest, PtraceEventStopWithFork) {
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (!test_helper::IsStarnix()) {
    GTEST_SKIP() << "This test does not work on Linux in CQ";
  }
  pid_t child_pid;
  ForkUsingClone3(false, 0, &child_pid);
  if (HasFatalFailure()) {
    return;
  }

  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) << " status " << status;
  ASSERT_EQ(0, ptrace(PTRACE_SETOPTIONS, child_pid, 0, PTRACE_O_TRACEFORK))
      << "error " << strerror(errno);

  DetectForkAndContinue(child_pid, false, true);
}

TEST(PtraceTest, PtraceEventStopWithForkAndSeize) {
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (!test_helper::IsStarnix()) {
    GTEST_SKIP() << "This test does not work on Linux in CQ";
  }
  pid_t child_pid;
  ForkUsingClone3(true, 0, &child_pid);
  if (HasFatalFailure()) {
    return;
  }

  ASSERT_EQ(ptrace(PTRACE_SEIZE, child_pid, 0, PTRACE_O_TRACEFORK), 0) << strerror(errno);
  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) << " status " << status;

  DetectForkAndContinue(child_pid, true, true);
}

TEST(PtraceTest, PtraceEventStopWithForkClonePtrace) {
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (!test_helper::IsStarnix()) {
    GTEST_SKIP() << "This test does not work on Linux in CQ";
  }
  pid_t child_pid;
  ForkUsingClone3(false, CLONE_PTRACE, &child_pid);
  if (HasFatalFailure()) {
    return;
  }
  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) << " status " << status;

  DetectForkAndContinue(child_pid, false, false);
}

TEST(PtraceTest, PtraceEventStopWithVForkClonePtrace) {
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (!test_helper::IsStarnix()) {
    GTEST_SKIP() << "This test does not work on Linux in CQ";
  }
  pid_t child_pid = fork();
  if (child_pid == 0) {
    ASSERT_EQ(ptrace(PTRACE_TRACEME, 0, 0, 0), 0);
    raise(SIGSTOP);
    pid_t grandchild_pid = vfork();
    if (grandchild_pid == 0) {
      exit(99);
    }
    int status;
    ASSERT_EQ(grandchild_pid, waitpid(grandchild_pid, &status, 0));
    ASSERT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 99)
        << "Failure: WIFEXITED(status) =" << WIFEXITED(status)
        << " WEXITSTATUS(status) == " << WEXITSTATUS(status);
    exit(0);
  }
  ASSERT_LT(0, child_pid);
  pid_t grandchild_pid;
  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) << " status " << status;
  ASSERT_EQ(0,
            ptrace(PTRACE_SETOPTIONS, child_pid, 0, PTRACE_O_TRACEVFORK | PTRACE_O_TRACEVFORKDONE));

  ASSERT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, 0))
      << strerror(errno) << ": with child pid " << child_pid;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));

  ASSERT_TRUE(WIFSTOPPED(status) && (status >> 8) == (SIGTRAP | (PTRACE_EVENT_VFORK << 8)))
      << "status = " << status;

  // Get the grandchild's pid as reported by ptrace
  ASSERT_EQ(0, get_event_msg<pid_t>(child_pid, &grandchild_pid)) << strerror(errno);
  ASSERT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, 0))
      << strerror(errno) << ": with child pid " << child_pid;

  // Let the grandchild continue.
  ASSERT_EQ(grandchild_pid, waitpid(grandchild_pid, &status, 0)) << strerror(errno);
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP) << " status " << status;
  // Child should not have made progress..
  ASSERT_EQ(0, waitpid(child_pid, &status, WNOHANG)) << strerror(errno);
  ASSERT_EQ(0, ptrace(PTRACE_CONT, grandchild_pid, 0, 0)) << strerror(errno);
  ASSERT_EQ(grandchild_pid, waitpid(grandchild_pid, &status, 0)) << strerror(errno);
  ASSERT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 99)
      << "WIFEXITED(status) == " << WIFEXITED(status)
      << " WEXITSTATUS(status) == " << WEXITSTATUS(status);

  // Grandchild is done, child should continue.
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0)) << strerror(errno);
  ASSERT_TRUE(WIFSTOPPED(status) && (status >> 8) == (SIGTRAP | (PTRACE_EVENT_VFORK_DONE << 8)))
      << "status = " << status;
  ASSERT_EQ(0, ptrace(PTRACE_DETACH, child_pid, 0, 0)) << strerror(errno);
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0)) << strerror(errno);
  ASSERT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0)
      << "WIFEXITED(status) == " << WIFEXITED(status)
      << " WEXITSTATUS(status) == " << WEXITSTATUS(status);
}

constexpr int kBadExitStatus = 0xabababab;

void DoExec(pid_t *out) {
  pid_t child_pid = fork();
  if (child_pid == 0) {
    ASSERT_EQ(ptrace(PTRACE_TRACEME, 0, 0, 0), 0) << strerror(errno);
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
  *out = child_pid;
}

// Ensure that the tracee sends a SIGTRAP when it encounters an exec and
// TRACEEXEC is not enabled.
TEST(PtraceTest, ExecveWithSigtrap) {
  pid_t child_pid;
  DoExec(&child_pid);

  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  ASSERT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, 0));

  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  ASSERT_EQ(0, ptrace(PTRACE_DETACH, child_pid, 0, 0));
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0)
      << "WIFEXITED(status) == " << WIFEXITED(status)
      << " WEXITSTATUS(status) == " << WEXITSTATUS(status);
}

// Ensure that, if TRACEEXIT is enabled, and the tracee executes an exit, it
// then sends a SIGTRAP | (PTRACE_EVENT_EXIT << 8)
TEST(PtraceTest, PtraceEventStopWithExit) {
  // TODO(https://fxbug.dev/322238868): This test does not work on the LTO
  // builder in CQ.
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (!test_helper::IsStarnix()) {
    GTEST_SKIP() << "This test does not work on Linux in CQ";
  }

  pid_t child_pid;
  DoExec(&child_pid);

  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  ASSERT_EQ(0, ptrace(PTRACE_SETOPTIONS, child_pid, 0, PTRACE_O_TRACEEXIT))
      << "error " << strerror(errno);
  ASSERT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, 0));

  // Wait for the exec
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);
  ASSERT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, 0));

  // Wait for the exit
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  ASSERT_EQ(SIGTRAP | (PTRACE_EVENT_EXIT << 8), status >> 8);
  int exit_status = kBadExitStatus;
  ASSERT_EQ(get_event_msg<int>(child_pid, &exit_status), 0);
  // The actual exit status seems to change depending on how this test is run,
  // so just make sure that something is returned.
  ASSERT_TRUE(kBadExitStatus != exit_status)
      << "expected = " << kBadExitStatus << " actual: " << exit_status;
  ASSERT_EQ(0, ptrace(PTRACE_DETACH, child_pid, 0, 0)) << " with child pid " << child_pid;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0)
      << "WIFEXITED(status) == " << WIFEXITED(status)
      << " WEXITSTATUS(status) == " << WEXITSTATUS(status);
}

// Ensure that, if TRACEEXEC is enabled, and the tracee executes an exec, it
// then sends a SIGTRAP | (PTRACE_EVENT_EXEC << 8).
TEST(PtraceTest, PtraceEventStopWithExecve) {
  // TODO(https://fxbug.dev/322238868): This test does not work on the LTO
  // builder in CQ.
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (!test_helper::IsStarnix()) {
    GTEST_SKIP() << "This test does not work on Linux in CQ";
  }
  pid_t child_pid;
  DoExec(&child_pid);

  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  ASSERT_EQ(0, ptrace(PTRACE_SETOPTIONS, child_pid, 0, PTRACE_O_TRACEEXEC | PTRACE_O_TRACEEXIT))
      << "error " << strerror(errno);
  ASSERT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, 0));

  // Wait for the exec
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  ASSERT_EQ(SIGTRAP | (PTRACE_EVENT_EXEC << 8), status >> 8);
  pid_t target_pid;
  ASSERT_EQ(get_event_msg<pid_t>(child_pid, &target_pid), 0);
  ASSERT_EQ(target_pid, child_pid);

  ASSERT_EQ(0, ptrace(PTRACE_DETACH, child_pid, 0, 0))
      << strerror(errno) << ": with child pid " << child_pid;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0)
      << "WIFEXITED(status) == " << WIFEXITED(status)
      << " WEXITSTATUS(status) == " << WEXITSTATUS(status);
}

// Ensure that, if TRACEEXIT is enabled, and the tracee is killed with a
// SIGTERM, it sends a SIGTRAP | (PTRACE_EVENT_EXIT << 8)
TEST(PtraceTest, PtraceEventStopWithSignalExit) {
  // TODO(https://fxbug.dev/322238868): This test does not work on the LTO
  // builder in CQ.
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (!test_helper::IsStarnix()) {
    GTEST_SKIP() << "This test does not work on Linux in CQ";
  }

  pid_t child_pid;
  DoExec(&child_pid);

  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  ASSERT_EQ(0, ptrace(PTRACE_SETOPTIONS, child_pid, 0, PTRACE_O_TRACEEXIT))
      << "error " << strerror(errno);
  ASSERT_EQ(0, kill(child_pid, SIGTERM));
  ASSERT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, 0));

  // Wait for the signal-delivery-stop
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGTERM)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);
  ASSERT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, SIGTERM));

  // Wait for the exit
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGTRAP)
      << "status = " << status << " WIFSTOPPED = " << WIFSTOPPED(status)
      << " WSTOPSIG = " << WSTOPSIG(status);

  ASSERT_EQ(SIGTRAP | (PTRACE_EVENT_EXIT << 8), status >> 8);
  int exit_status = 0xabababab;
  ASSERT_EQ(get_event_msg<int>(child_pid, &exit_status), 0);
  ASSERT_TRUE(SIGTERM == exit_status) << " exit_status " << exit_status;
  ASSERT_EQ(0, ptrace(PTRACE_DETACH, child_pid, 0, 0))
      << strerror(errno) << " with child pid " << child_pid;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSIGNALED(status) && WTERMSIG(status) == SIGTERM)
      << "WIFSIGNALED(status) == " << WIFEXITED(status)
      << " WTERMSIG(status) == " << WTERMSIG(status);
}

namespace {
void GrandchildWithSigsuspendSigaction(int, siginfo_t *, void *) {
  // NOP
}
}  // namespace

// Test that traced child correctly resumes when signal needs to be delivered
// because of a temporary mask.
TEST(PtraceTest, GrandchildWithSigsuspend) {
  // TODO(https://fxbug.dev/317285180) don't skip on baseline
  if (!test_helper::IsStarnix()) {
    GTEST_SKIP() << "This test does not work on Linux in CQ";
  }
  test_helper::ForkHelper helper;
  helper.OnlyWaitForForkedChildren();
  pid_t child_pid = helper.RunInForkedProcess([] {
    ASSERT_EQ(0, ptrace(PTRACE_TRACEME, 0, 0, 0));
    ASSERT_EQ(0, raise(SIGSTOP));
    sigset_t child_mask, old_mask;
    ASSERT_EQ(0, sigemptyset(&child_mask));
    ASSERT_EQ(0, sigaddset(&child_mask, SIGCHLD));

    sigset_t empty_mask;
    ASSERT_EQ(0, sigemptyset(&empty_mask));
    struct sigaction sa, oldact;
    sa.sa_sigaction = GrandchildWithSigsuspendSigaction;
    sa.sa_mask = empty_mask;
    ASSERT_EQ(0, sigaction(SIGCHLD, &sa, &oldact));
    pid_t my_pid = getpid();
    ASSERT_EQ(0, sigprocmask(SIG_BLOCK, &child_mask, &old_mask));
    pid_t gc_pid = fork();
    if (gc_pid == 0) {
      test_helper::WaitUntilBlocked(my_pid, false);
      exit(0);
    }
    ASSERT_EQ(-1, sigsuspend(&old_mask));
    int status;
    ASSERT_EQ(gc_pid, waitpid(gc_pid, &status, 0));
    ASSERT_TRUE(WIFEXITED(status) && WEXITSTATUS(status) == 0)
        << "WIFEXITED(status) == " << WIFEXITED(status)
        << " WEXITSTATUS(status) == " << WEXITSTATUS(status);
  });
  int status;
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGSTOP)
      << WIFSTOPPED(status) << " " << WSTOPSIG(status);
  ASSERT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, 0));
  ASSERT_EQ(child_pid, waitpid(child_pid, &status, 0));
  ASSERT_TRUE(WIFSTOPPED(status) && WSTOPSIG(status) == SIGCHLD)
      << WIFSTOPPED(status) << " " << WSTOPSIG(status);
  ASSERT_EQ(0, ptrace(PTRACE_CONT, child_pid, 0, SIGCHLD));
}
