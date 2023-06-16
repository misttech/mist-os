// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/starnix/tests/syscalls/task_test.h"

#include <limits.h>
#include <sched.h>
#include <strings.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/types.h>
#include <sys/wait.h>
#include <unistd.h>

#include <cerrno>
#include <cstdint>
#include <thread>

#include <gtest/gtest.h>
#include <linux/sched.h>

#include "src/lib/files/directory.h"
#include "src/starnix/tests/syscalls/test_helper.h"

namespace {

#define MAX_PAGE_ALIGNMENT (1 << 21)

// Unless the split between the data and bss sections happens to be page-aligned, initial part
// of the bss section will be in the same page as the last part of the data section.
// By aligning g_global_variable_bss to a value bigger than the page size, we prevent it from
// ending up in this shared page
alignas(MAX_PAGE_ALIGNMENT) volatile int g_global_variable_bss = 0;

volatile size_t g_global_variable_data = 15;
volatile size_t g_fork_doesnt_drop_writes = 0;

// As of this writing, our sysroot's syscall.h lacks the SYS_clone3 definition.
#ifndef SYS_clone3
#if defined(__aarch64__) || defined(__x86_64__)
#define SYS_clone3 435
#else
#error SYS_clone3 needs a definition for this architecture.
#endif
#endif

constexpr int kChildExpectedExitCode = 21;
constexpr int kChildErrorExitCode = kChildExpectedExitCode + 1;

pid_t ForkUsingClone3(const clone_args* cl_args, size_t size) {
  return static_cast<pid_t>(syscall(SYS_clone3, cl_args, size));
}

// calls clone3 and executes a function, calling exit with its return value.
pid_t DoClone3(const clone_args* cl_args, size_t size, int (*func)(void*), void* param) {
  pid_t pid;
  // clone3 lets you specify a new stack, but not which function to run.
  // This means that after the clone3 syscall, the child will be running on a
  // new stack, not being able to access any local variables from before the
  // clone.
  //
  // We have to manually call into the new function in assembly, being careful
  // to not refer to any variables from the stack.
#if defined(__aarch64__)
  __asm__ volatile(
      "mov x0, %[cl_args]\n"
      "mov x1, %[size]\n"
      "mov w8, %[clone3]\n"
      "svc #0\n"
      "cbnz x0, 1f\n"
      "mov x0, %[param]\n"
      "blr %[func]\n"
      "mov w8, %[exit]\n"
      "svc #0\n"
      "brk #1\n"
      "1:\n"
      "mov %w[pid], w0\n"
      : [pid] "=r"(pid)
      : [cl_args] "r"(cl_args), "m"(*cl_args), [size] "r"(size), [func] "r"(func),
        [param] "r"(param), [clone3] "i"(SYS_clone3), [exit] "i"(SYS_exit)
      : "x0", "x1", "x2", "x3", "x4", "x5", "x6", "x7", "x8", "x9", "x10", "x11", "x12", "x13",
        "x14", "x15", "x16", "x17", "cc", "memory");
#elif defined(__x86_64__)
  __asm__ volatile(
      "syscall\n"
      "test %%rax, %%rax\n"
      "jnz 1f\n"
      "movq %[param], %%rdi\n"
      "callq *%[func]\n"
      "movl %%eax, %%edi\n"
      "movl %[exit], %%eax\n"
      "syscall\n"
      "ud2\n"
      "1:\n"
      "movl %%eax, %[pid]\n"
      : [pid] "=g"(pid)
      : "D"(cl_args), "m"(*cl_args), "S"(size),
        "a"(SYS_clone3), [func] "r"(func), [param] "r"(param), [exit] "i"(SYS_exit)
      : "rcx", "rdx", "r8", "r9", "r10", "r11", "cc", "memory");
#else
#error clone3 needs a manual asm wrapper.
#endif

  if (pid < 0) {
    errno = -pid;
    pid = -1;
  }
  return pid;
}

int stack_test_func(void* a) {
  // Force a stack write by creating an asm block
  // that has an input that needs to come from memory.
  int pid = *reinterpret_cast<int*>(a);
  __asm__("" ::"m"(pid));

  if (getpid() != pid)
    return kChildErrorExitCode;

  return kChildExpectedExitCode;
}

int empty_func(void*) { return 0; }

}  // namespace

// Creates a child process using the "clone3()" syscall and waits on it.
// The child uses a different stack than the parent.
TEST(Task, Clone3_ChangeStack) {
  struct clone_args ca;
  bzero(&ca, sizeof(ca));

  ca.flags = CLONE_PARENT_SETTID | CLONE_CHILD_SETTID;
  ca.exit_signal = SIGCHLD;  // Needed in order to wait on the child.

  // Ask for the child PID to be reported to both the parent and the child for validation.
  uint64_t child_pid_from_clone = 0;
  ca.parent_tid = reinterpret_cast<uint64_t>(&child_pid_from_clone);
  ca.child_tid = reinterpret_cast<uint64_t>(&child_pid_from_clone);

  constexpr size_t kStackSize = 0x5000;
  void* stack_addr = mmap(NULL, kStackSize, PROT_WRITE | PROT_READ,
                          MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK, -1, 0);
  ASSERT_NE(MAP_FAILED, stack_addr);

  ca.stack = reinterpret_cast<uint64_t>(stack_addr);
  ca.stack_size = kStackSize;

  auto child_pid = DoClone3(&ca, sizeof(ca), &stack_test_func, &child_pid_from_clone);
  ASSERT_NE(child_pid, -1);

  EXPECT_EQ(static_cast<pid_t>(child_pid_from_clone), child_pid);

  // Wait for the child to terminate and validate the exit code. Note that it returns a different
  // exit code above to indicate its state wasn't as expected.
  int wait_status = 0;
  pid_t wait_result = waitpid(child_pid, &wait_status, 0);
  EXPECT_EQ(wait_result, child_pid);

  EXPECT_TRUE(WIFEXITED(wait_status));
  auto exit_status = WEXITSTATUS(wait_status);
  EXPECT_NE(exit_status, kChildErrorExitCode) << "Child process reported state was unexpected.";
  EXPECT_EQ(exit_status, kChildExpectedExitCode) << "Wrong exit code from child process.";

  ASSERT_EQ(0, munmap(stack_addr, kStackSize));
}

// Forks a child process using the "clone3()" syscall and waits on it, validating some parameters.
TEST(Task, Clone3_Fork) {
  struct clone_args ca;
  bzero(&ca, sizeof(ca));

  ca.flags = CLONE_PARENT_SETTID | CLONE_CHILD_SETTID;
  ca.exit_signal = SIGCHLD;  // Needed in order to wait on the child.

  // Ask for the child PID to be reported to both the parent and the child for validation.
  uint64_t child_pid_from_clone = 0;
  ca.parent_tid = reinterpret_cast<uint64_t>(&child_pid_from_clone);
  ca.child_tid = reinterpret_cast<uint64_t>(&child_pid_from_clone);

  auto child_pid = ForkUsingClone3(&ca, sizeof(ca));
  ASSERT_NE(child_pid, -1);
  if (child_pid == 0) {
    // In child process. We'd like to EXPECT_EQ the pid but this is a child process and the gtest
    // failure won't get caught. Instead, return a different result code and the parent will notice
    // and issue an error about the state being unexpected.
    if (getpid() != static_cast<pid_t>(child_pid_from_clone))
      exit(kChildErrorExitCode);
    exit(kChildExpectedExitCode);
  } else {
    EXPECT_EQ(static_cast<pid_t>(child_pid_from_clone), child_pid);

    // Wait for the child to terminate and validate the exit code. Note that it returns a different
    // exit code above to indicate its state wasn't as expected.
    int wait_status = 0;
    pid_t wait_result = waitpid(child_pid, &wait_status, 0);
    EXPECT_EQ(wait_result, child_pid);

    EXPECT_TRUE(WIFEXITED(wait_status));
    auto exit_status = WEXITSTATUS(wait_status);
    EXPECT_NE(exit_status, kChildErrorExitCode) << "Child process reported state was unexpected.";
    EXPECT_EQ(exit_status, kChildExpectedExitCode) << "Wrong exit code from child process.";
  }
}

TEST(Task, Clone3_InvalidSize) {
  struct clone_args ca;
  bzero(&ca, sizeof(ca));

  // Pass a structure size smaller than the first supported version, it should report EINVAL.
  EXPECT_EQ(-1, DoClone3(&ca, CLONE_ARGS_SIZE_VER0 - 8, &empty_func, NULL));
  EXPECT_EQ(EINVAL, errno);
}

static int CloneVForkFunctionSleepExit(void* param) {
  struct timespec wait {
    .tv_sec = 0, .tv_nsec = kCloneVforkSleepUS * 1000
  };
  nanosleep(&wait, nullptr);
  // Note: exit() is a stdlib function that exits the whole process which we don't want.
  // _exit just exits the current thread which is what matches clone().
  _exit(1);
  return 0;
}

// Tests a CLONE_VFORK and the cloned thread exits before calling execve. The clone() call should
// block until the thread exits.
TEST(Task, CloneVfork_exit) {
  constexpr size_t kStackSize = 1024 * 16;
  void* stack_low = mmap(NULL, kStackSize, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK, -1, 0);
  ASSERT_NE(stack_low, MAP_FAILED);
  void* stack_high = static_cast<char*>(stack_low) + kStackSize;  // Pass in the top of the stack.

  struct timeval begin;
  struct timezone tz;
  gettimeofday(&begin, &tz);

  // This uses the glibc "clone()" wrapper function which takes a function pointer.
  int result = clone(&CloneVForkFunctionSleepExit, stack_high, CLONE_VFORK, 0);
  ASSERT_NE(result, -1);

  struct timeval end;
  gettimeofday(&end, &tz);

  // The clone function should have been blocked for at least as long as the sleep was for.
  uint64_t elapsed_us = ((int64_t)end.tv_sec - (int64_t)begin.tv_sec) * 1000000ll +
                        ((int64_t)end.tv_usec - (int64_t)begin.tv_usec);
  EXPECT_GT(elapsed_us, kCloneVforkSleepUS);
}

TEST(Task, BrkShrinkAfterFork) {
  // Tests that a program can shrink their break after forking.
  const void* SBRK_ERROR = reinterpret_cast<void*>(-1);
  ForkHelper helper;

  constexpr int brk_increment = 0x4000;
  ASSERT_NE(SBRK_ERROR, sbrk(brk_increment));
  void* old_brk = sbrk(0);
  ASSERT_NE(SBRK_ERROR, old_brk);
  helper.RunInForkedProcess([&] {
    ASSERT_EQ(old_brk, sbrk(0));
    ASSERT_NE(SBRK_ERROR, sbrk(-brk_increment));
  });

  // Make sure fork didn't change our current break.
  ASSERT_EQ(old_brk, sbrk(0));
  // Restore the old brk.
  ASSERT_NE(SBRK_ERROR, sbrk(-brk_increment));
}

TEST(Task, ChildCantModifyParent) {
  ASSERT_GT(MAX_PAGE_ALIGNMENT, getpagesize());

  ForkHelper helper;

  g_global_variable_data = 1;
  g_global_variable_bss = 10;
  volatile int local_variable = 100;
  volatile int* heap_variable = new volatile int();
  *heap_variable = 1000;

  ASSERT_EQ(g_global_variable_data, 1u);
  ASSERT_EQ(g_global_variable_bss, 10);
  ASSERT_EQ(local_variable, 100);
  ASSERT_EQ(*heap_variable, 1000);

  helper.RunInForkedProcess([&] {
    g_global_variable_data = 2;
    g_global_variable_bss = 20;
    local_variable = 200;
    *heap_variable = 2000;
  });
  ASSERT_TRUE(helper.WaitForChildren());

  EXPECT_EQ(g_global_variable_data, 1u);
  EXPECT_EQ(g_global_variable_bss, 10);
  EXPECT_EQ(local_variable, 100);
  EXPECT_EQ(*heap_variable, 1000);
  delete heap_variable;
}

TEST(Task, ForkDoesntDropWrites) {
  // This tests creates a thread that keeps reading
  // and writing to a pager-backed memory region (the data section of this
  // binary).
  //
  // This is to test that during fork, writes to pager-backed vmos are not
  // dropped if we have concurrent processes writing to memory while the kernel
  // changes those mappings.
  std::atomic<bool> stop = false;
  std::atomic<bool> success = false;
  std::vector<pid_t> pids;

  std::thread writer([&stop, &success, &data = g_fork_doesnt_drop_writes]() {
    data = 0;
    size_t i = 0;
    success = false;
    while (!stop) {
      i++;
      data += 1;
      if (data != i) {
        return;
      }
    }
    success = true;
  });

  for (size_t i = 0; i < 1000; i++) {
    pid_t pid = fork();
    if (pid == 0) {
      _exit(0);
    }
    pids.push_back(pid);
  }

  stop = true;
  writer.join();
  EXPECT_TRUE(success.load());

  for (auto pid : pids) {
    int status;
    EXPECT_EQ(waitpid(pid, &status, 0), pid);
    EXPECT_TRUE(WIFEXITED(status));
    EXPECT_EQ(WEXITSTATUS(status), 0);
  }
}

TEST(Task, ParentCantModifyChild) {
  ASSERT_GT(MAX_PAGE_ALIGNMENT, getpagesize());

  ForkHelper helper;

  g_global_variable_data = 1;
  g_global_variable_bss = 10;
  volatile int local_variable = 100;
  volatile int* heap_variable = new volatile int();
  *heap_variable = 1000;

  ASSERT_EQ(g_global_variable_data, 1u);
  ASSERT_EQ(g_global_variable_bss, 10);
  ASSERT_EQ(local_variable, 100);
  ASSERT_EQ(*heap_variable, 1000);

  SignalMaskHelper signal_helper = SignalMaskHelper();
  signal_helper.blockSignal(SIGUSR1);

  pid_t child_pid = helper.RunInForkedProcess([&] {
    signal_helper.waitForSignal(SIGUSR1);

    EXPECT_EQ(g_global_variable_data, 1u);
    EXPECT_EQ(g_global_variable_bss, 10);
    EXPECT_EQ(local_variable, 100);
    EXPECT_EQ(*heap_variable, 1000);
  });

  g_global_variable_data = 2;
  g_global_variable_bss = 20;
  local_variable = 200;
  *heap_variable = 2000;

  ASSERT_EQ(kill(child_pid, SIGUSR1), 0);

  ASSERT_TRUE(helper.WaitForChildren());
  signal_helper.restoreSigmask();

  delete heap_variable;
}

constexpr size_t kVecSize = 100;
constexpr size_t kPageLimit = 32;

TEST(Task, ExecveArgumentExceedsMaxArgStrlen) {
  ForkHelper helper;
  helper.RunInForkedProcess([] {
    size_t arg_size = kPageLimit * sysconf(_SC_PAGESIZE);
    std::vector<char> arg(arg_size + 1, 'a');
    arg[arg_size] = '\0';
    char* argv[] = {arg.data(), nullptr};
    char* envp[] = {nullptr};
    EXPECT_NE(execve("/proc/self/exe", argv, envp), 0);
    EXPECT_EQ(errno, E2BIG);
  });
  ASSERT_TRUE(helper.WaitForChildren());
}

TEST(Task, ExecveArgvExceedsLimit) {
  ForkHelper helper;
  helper.RunInForkedProcess([] {
    size_t arg_size = kPageLimit * sysconf(_SC_PAGESIZE);
    std::vector<char> arg(arg_size, 'a');
    arg[arg_size - 1] = '\0';
    char* argv[kVecSize];
    std::fill_n(argv, kVecSize - 1, arg.data());
    argv[kVecSize - 1] = nullptr;
    char* envp[] = {nullptr};
    EXPECT_NE(execve("/proc/self/exe", argv, envp), 0);
    EXPECT_EQ(errno, E2BIG);
  });
  ASSERT_TRUE(helper.WaitForChildren());
}

TEST(Task, ExecveArgvEnvExceedLimit) {
  ForkHelper helper;
  helper.RunInForkedProcess([] {
    size_t arg_size = kPageLimit * sysconf(_SC_PAGESIZE);
    std::vector<char> string(arg_size, 'a');
    string[arg_size - 1] = '\0';
    char* argv[] = {string.data(), nullptr};
    char* envp[kVecSize];
    std::fill_n(envp, kVecSize - 1, string.data());
    envp[kVecSize - 1] = nullptr;
    EXPECT_NE(execve("/proc/self/exe", argv, envp), 0);
    EXPECT_EQ(errno, E2BIG);
  });
  ASSERT_TRUE(helper.WaitForChildren());
}

TEST(Task, ExecvePathnameTooLong) {
  ForkHelper helper;
  helper.RunInForkedProcess([] {
    constexpr size_t path_size = PATH_MAX + 1;
    // We use '/' here because ////// (...) is a valid path:
    // More than two leading / should be considered as one,
    // So this path resolves to "/".
    //
    // Each path component is limited to NAME_MAX, so if we
    // use a different character we would need to add a delimiter
    // every NAME_MAX characters.
    std::vector<char> pathname(path_size, '/');
    pathname[path_size - 1] = '\0';

    char* argv[] = {NULL};
    char* envp[] = {NULL};

    EXPECT_NE(execve(pathname.data(), argv, envp), 0);
    EXPECT_EQ(errno, ENAMETOOLONG);
  });

  ASSERT_TRUE(helper.WaitForChildren());
}
