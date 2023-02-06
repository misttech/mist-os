// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/proc/tests/chromiumos/syscalls/task_test.h"

#include <fcntl.h>
#include <sched.h>
#include <strings.h>
#include <sys/mman.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <sys/wait.h>
#include <unistd.h>

#include <cstdint>

#include <gtest/gtest.h>
#include <linux/sched.h>

#include "src/lib/files/directory.h"

namespace {

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

// Returns the full path to the syscall_test_exec_child binary. This is in a directory named
// according to the hash of the package which is inconvenient. Returns empty string on failure.
//
// The location is /galaxy/pkg/<hash>/data/tests/syscall_test_exec_child
std::string GetTestExecChildBinary() {
  const char kPkgDir[] = "/galaxy/pkg";
  const char kInPackage[] = "/data/tests/syscall_test_exec_child";

  std::vector<std::string> subs;
  if (!files::ReadDirContents(kPkgDir, &subs))
    return std::string();

  for (const auto& cur : subs) {
    if (cur == "." || cur == "..")
      continue;

    // Assume the first (normally only) child of the package directory is ours.
    return std::string(kPkgDir) + "/" + cur + kInPackage;
  }

  return std::string();
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

// Calls execve() on the binary whose null-terminated string is passed in as the parameter.
static int CloneVForkFunctionExec(void* void_binary_cstring) {
  char* binary_name = (char*)void_binary_cstring;
  char* argv[2] = {binary_name, nullptr};
  char* envp[1] = {nullptr};
  execve(binary_name, argv, envp);
  return 0;
}

// Tests a CLONE_VFORK followed by a successful call to execve() which should unblock clone().
TEST(Task, CloneVfork_execve) {
  std::string test_binary = GetTestExecChildBinary();
  ASSERT_FALSE(test_binary.empty());

  constexpr size_t kStackSize = 1024 * 16;
  void* stack_low = mmap(NULL, kStackSize, PROT_READ | PROT_WRITE,
                         MAP_PRIVATE | MAP_ANONYMOUS | MAP_STACK, -1, 0);
  ASSERT_NE(stack_low, MAP_FAILED);
  void* stack_high = static_cast<char*>(stack_low) + kStackSize;  // Pass in the top of the stack.

  // Manually copy the string to ensure the pointer is valid after cloned.
  size_t binary_name_buf_len = test_binary.size() + 1;
  char* binary_string = (char*)malloc(binary_name_buf_len);
  strncpy(binary_string, test_binary.c_str(), binary_name_buf_len);

  struct timeval begin;
  struct timezone tz;
  gettimeofday(&begin, &tz);

  // This uses the glibc "clone()" wrapper function which takes a function pointer.
  int result = clone(&CloneVForkFunctionExec, stack_high, CLONE_VFORK, binary_string);
  ASSERT_NE(result, -1) << "errno = " << errno;

  struct timeval end;
  gettimeofday(&end, &tz);

  // The clone function should have been blocked for at least as long as the sleep was for.
  uint64_t elapsed_us = ((int64_t)end.tv_sec - (int64_t)begin.tv_sec) * 1000000ll +
                        ((int64_t)end.tv_usec - (int64_t)begin.tv_usec);
  EXPECT_GT(elapsed_us, kCloneVforkSleepUS);
}

TEST(Task, Vfork) {
  std::string test_binary = GetTestExecChildBinary();
  ASSERT_FALSE(test_binary.empty());

  struct timeval begin;
  struct timezone tz;
  gettimeofday(&begin, &tz);

  // This uses the glibc "clone()" wrapper function which takes a function pointer.
  auto result = vfork();
  ASSERT_NE(result, -1);
  if (result == 0) {
    // In the forked child process.
    char* binary_str = const_cast<char*>(test_binary.c_str());
    char* argv[2] = {binary_str, nullptr};
    char* envp[1] = {nullptr};
    execve(binary_str, argv, envp);
  }

  struct timeval end;
  gettimeofday(&end, &tz);

  // The clone function should have been blocked for at least as long as the sleep was for.
  uint64_t elapsed_us = ((int64_t)end.tv_sec - (int64_t)begin.tv_sec) * 1000000ll +
                        ((int64_t)end.tv_usec - (int64_t)begin.tv_usec);
  EXPECT_GT(elapsed_us, kCloneVforkSleepUS);
}
