// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/starnix/tests/syscalls/cpp/task_test.h"

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

#include <fbl/algorithm.h>
#include <gtest/gtest.h>
#include <linux/sched.h>

#include "src/lib/files/directory.h"
#include "src/lib/files/file.h"
#include "src/lib/fxl/strings/split_string.h"
#include "src/lib/fxl/strings/string_number_conversions.h"
#include "src/lib/fxl/strings/string_printf.h"
#include "src/starnix/tests/syscalls/cpp/test_helper.h"

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
#if defined(__aarch64__) || defined(__x86_64__) || defined(__riscv)
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
#elif defined(__arm__)
  __asm__ volatile(
      "mov r0, %[cl_args]\n"
      "mov r1, %[size]\n"
      "mov r7, %[clone3]\n"
      "svc #0\n"
      "cmp r0, #0\n"
      "bne 1f\n"
      "mov r0, %[param]\n"
      "blx %[func]\n"
      "mov r7, %[exit]\n"
      "svc #0\n"
      "bkpt\n"
      "1:\n"
      "mov %[pid], r0\n"
      : [pid] "=r"(pid)
      : [cl_args] "r"(cl_args), "m"(*cl_args), [size] "r"(size), [func] "r"(func),
        [param] "r"(param), [clone3] "i"(SYS_clone3), [exit] "i"(SYS_exit)
      : "r0", "r1", "r2", "r3", "r7", "r12", "lr", "cc", "memory");
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
#elif defined(__riscv)
  __asm__ volatile(
      "mv   a0, %[cl_args]\n"
      "mv   a1, %[size]\n"
      "li   a7, %[sys_clone3]\n"
      "ecall\n"
      "bnez a0, 1f\n"
      "mv   a0, %[param]\n"
      "jalr %[func]\n"
      "li   a7, %[sys_exit]\n"
      "ecall\n"
      "ebreak\n"
      "1:\n"
      "mv   %[pid], a0\n"
      : [pid] "=r"(pid)
      : [cl_args] "r"(cl_args), "m"(*cl_args), [size] "r"(size), [func] "r"(func),
        [param] "r"(param), [sys_clone3] "i"(SYS_clone3), [sys_exit] "i"(SYS_exit)
      : "ra", "t0", "t1", "t2", "t3", "t4", "t5", "t6", "a0", "a1", "a2", "a3", "a4", "a5", "a6",
        "a7", "s1", "memory");
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

void ReadStackStart(pid_t pid, uintptr_t* result) {
  std::string contents, path = fxl::StringPrintf("/proc/%ld/stat", static_cast<long>(pid));
  ASSERT_TRUE(files::ReadFileToString(path, &contents));

  auto parts = fxl::SplitString(contents, " ", fxl::kTrimWhitespace, fxl::kSplitWantAll);
  ASSERT_GE(parts.size(), 28U) << contents;
  ASSERT_TRUE(fxl::StringToNumberWithError(parts[27], result)) << parts[27];
}

void ReadStackSize(pid_t pid, uintptr_t* result) {
  std::string contents, path = fxl::StringPrintf("/proc/%ld/status", static_cast<long>(pid));
  ASSERT_TRUE(files::ReadFileToString(path, &contents));
  std::vector<std::string_view> lines =
      fxl::SplitString(contents, "\n", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);

  bool vm_stk_found = false;
  for (auto line : lines) {
    auto key_and_value =
        fxl::SplitString(line, ": ", fxl::kTrimWhitespace, fxl::kSplitWantNonEmpty);
    if (key_and_value.size() >= 2 && key_and_value[0] == "VmStk") {
      ASSERT_TRUE(fxl::StringToNumberWithError(key_and_value[1], result)) << line;
      vm_stk_found = true;
      break;
    }
  }

  ASSERT_TRUE(vm_stk_found) << contents;
}

void ReadProcPidFile(pid_t pid, const char* name, std::vector<uint8_t>* result) {
  std::string path = fxl::StringPrintf("/proc/%ld/%s", static_cast<long>(pid), name);
  ASSERT_TRUE(files::ReadFileToVector(path, result));
}

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
  struct timespec wait{.tv_sec = 0, .tv_nsec = kCloneVforkSleepUS * 1000};
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

// Invokes `brk()` syscall directly. The syscall returns the requested `new_break` on success,
// or the old break on failure. Setting `new_break` to null always fails, so can be used to
// retrieve the current break.
// Some tests invoke this directly so as to bypass the `brk()` wrapper provided by some libc
// implementations, that has different behaviour, matching the POSIX specification.
uintptr_t brk_syscall(uintptr_t new_break) { return syscall(SYS_brk, new_break); }

TEST(Task, BrkReturnsCurrentBreakOnFailure) {
  // Tests that the brk system call doesn't return an error, instead it returns
  // the current value of the program break.
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([&] {
    const size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));

    // This should always fail, returning the program_break
    uintptr_t program_break = brk_syscall(UINTPTR_MAX);

    // Try to reserve something beyond the program break, aligned to page size.
    uintptr_t map_addr = (program_break & ~(page_size - 1)) + page_size;
    uintptr_t res =
        reinterpret_cast<uintptr_t>(mmap(reinterpret_cast<void*>(map_addr), page_size, PROT_NONE,
                                         MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED_NOREPLACE, -1, 0));
    ASSERT_EQ(res, map_addr);

    uintptr_t new_break = brk_syscall(map_addr + page_size);
    // brk should fail.
    EXPECT_EQ(new_break, program_break);

    _exit(testing::Test::HasFailure());
  });

  EXPECT_TRUE(helper.WaitForChildren());
}

TEST(Task, BrkShrinkAfterFork) {
  // Tests that a program can shrink their break after forking.
  const void* kSbrkError = reinterpret_cast<void*>(-1);
  test_helper::ForkHelper helper;

  constexpr int brk_increment = 0x4000;
  ASSERT_NE(kSbrkError, sbrk(brk_increment));
  void* old_brk = sbrk(0);
  ASSERT_NE(kSbrkError, old_brk);
  helper.RunInForkedProcess([&] {
    ASSERT_EQ(old_brk, sbrk(0));
    ASSERT_NE(kSbrkError, sbrk(-brk_increment));
  });

  // Make sure fork didn't change our current break.
  ASSERT_EQ(old_brk, sbrk(0));

  // Restore the old brk.
  ASSERT_NE(kSbrkError, sbrk(-brk_increment));
}

// Returns true if the `len` bytes at `ptr` all have value `value`.
static bool mem_contains(volatile char* ptr, size_t len, char value) {
  for (const auto* end = ptr + len; ptr != end; ++ptr) {
    if (*ptr != value)
      return false;
  }
  return true;
}

// Moves the program break to a page aligned address, and returns the old break address.
static uintptr_t set_brk_page_aligned() {
  size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));
  uintptr_t original_break = brk_syscall(0);
  uintptr_t aligned_break = (original_break + page_size) & ~(page_size - 1);
  if (brk_syscall(aligned_break) != aligned_break) {
    return 0;
  }
  return original_break;
}

// Verify that if the program break is reduced down to a page-aligned address, and then
// regrown, then all the pages, including the one pointed to by the page-aligned address,
// are zeroed.
TEST(Task, BrkIsZeroedAfterShrinkAndRegrow) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([&] {
    const void* kSbrkError = reinterpret_cast<void*>(-1);

    ASSERT_NE(set_brk_page_aligned(), 0u);

    // Allocate space via `sbrk()`, and fill it with non-zero bytes.
    constexpr intptr_t kBreakIncrement = 0x4142;
    void* base_break = sbrk(kBreakIncrement);
    ASSERT_NE(base_break, kSbrkError) << "sbrk failed: " << std::strerror(errno);
    char* memory = static_cast<char*>(base_break);
    ASSERT_TRUE(mem_contains(memory, static_cast<size_t>(kBreakIncrement), 0));
    memset(memory, 'a', kBreakIncrement);
    ASSERT_FALSE(mem_contains(memory, static_cast<size_t>(kBreakIncrement), 0));

    // Shrink the `sbrk()` to free the pages, then re-allocate them.
    ASSERT_NE(sbrk(-kBreakIncrement), kSbrkError) << "sbrk failed: " << std::strerror(errno);
    ASSERT_NE(sbrk(kBreakIncrement), kSbrkError) << "sbrk failed: " << std::strerror(errno);

    // The re-allocated range should now be zeroed.
    EXPECT_TRUE(mem_contains(memory, static_cast<size_t>(kBreakIncrement), 0));

    _exit(testing::Test::HasFailure());
  });
}

// Verifies that overwriting the program break with private or shared mappings does not
// prevent the program break from being shrunk, and regrown.
TEST(Task, BrkShrinkUnmapsAnonMmap) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([&] {
    ASSERT_NE(set_brk_page_aligned(), 0u);

    size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));

    // Allocate eight pages from the program break.
    uintptr_t program_break = brk_syscall(0);
    uintptr_t new_break = brk_syscall(program_break + (8 * page_size));
    ASSERT_EQ(new_break, program_break + (8 * page_size));

    // Anonymously map over second and third pages, with RW and read-only private pages.
    uintptr_t addr = reinterpret_cast<uintptr_t>(
        mmap(reinterpret_cast<void*>(program_break + page_size), page_size, PROT_WRITE | PROT_READ,
             MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0));
    ASSERT_EQ(addr, program_break + page_size);
    addr = reinterpret_cast<uintptr_t>(
        mmap(reinterpret_cast<void*>(program_break + (2 * page_size)), page_size, PROT_READ,
             MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0));
    ASSERT_EQ(addr, program_break + (2 * page_size));

    // Anonymously map over fifth and sixth pages, with RW and read-only shared pages.
    addr = reinterpret_cast<uintptr_t>(
        mmap(reinterpret_cast<void*>(program_break + (4 * page_size)), page_size,
             PROT_WRITE | PROT_READ, MAP_SHARED | MAP_ANONYMOUS | MAP_FIXED, -1, 0));
    ASSERT_EQ(addr, program_break + (4 * page_size));
    addr = reinterpret_cast<uintptr_t>(
        mmap(reinterpret_cast<void*>(program_break + (5 * page_size)), page_size, PROT_READ,
             MAP_SHARED | MAP_ANONYMOUS | MAP_FIXED, -1, 0));
    ASSERT_EQ(addr, program_break + (5 * page_size));

    // Shrink the program break back down.
    uintptr_t final_break = brk_syscall(program_break);
    EXPECT_EQ(final_break, program_break);

    // Validate that all eight pages are no longer available.
    for (size_t i = 0; i < 8; i++) {
      EXPECT_FALSE(test_helper::TryRead(program_break + (i * page_size)))
          << "page " << i + 1 << " is readable";
    }

    // Regrow and validate that all eight pages are then readable.
    new_break = brk_syscall(program_break + (8 * page_size));
    EXPECT_EQ(new_break, program_break + (8 * page_size));
    for (size_t i = 0; i < 8; i++) {
      EXPECT_TRUE(test_helper::TryRead(program_break + (i * page_size)))
          << "page " << i + 1 << " not readable";
    }

    _exit(testing::Test::HasFailure());
  });
}

// Verifies that the program break can be extended after `mmap()` has been used to overwrite
// preceding parts of the program break area, and that doing so does not alter those
// mappings.
TEST(Task, BrkGrowsAfterAnonMmap) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([&] {
    ASSERT_NE(set_brk_page_aligned(), 0u);

    size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));

    // Allocate eight pages from the program break, and set them non-zero.
    uintptr_t program_break = brk_syscall(0);
    uintptr_t new_break = brk_syscall(program_break + (8 * page_size));
    ASSERT_EQ(new_break, program_break + (8 * page_size));

    char* break_memory = reinterpret_cast<char*>(program_break);
    memset(break_memory, 'a', 8 * page_size);
    ASSERT_TRUE(mem_contains(break_memory, 8 * page_size, 'a'));

    // Anonymously map over second and third pages, with RW and read-only private pages.
    uintptr_t addr = reinterpret_cast<uintptr_t>(
        mmap(reinterpret_cast<void*>(program_break + page_size), page_size, PROT_WRITE | PROT_READ,
             MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0));
    ASSERT_EQ(addr, program_break + page_size);
    addr = reinterpret_cast<uintptr_t>(
        mmap(reinterpret_cast<void*>(program_break + (2 * page_size)), page_size, PROT_READ,
             MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0));
    ASSERT_EQ(addr, program_break + (2 * page_size));

    // Anonymously map over fifth and sixth pages, with RW and read-only shared pages.
    addr = reinterpret_cast<uintptr_t>(
        mmap(reinterpret_cast<void*>(program_break + (4 * page_size)), page_size,
             PROT_WRITE | PROT_READ, MAP_SHARED | MAP_ANONYMOUS | MAP_FIXED, -1, 0));
    ASSERT_EQ(addr, program_break + (4 * page_size));
    addr = reinterpret_cast<uintptr_t>(
        mmap(reinterpret_cast<void*>(program_break + (5 * page_size)), page_size, PROT_READ,
             MAP_SHARED | MAP_ANONYMOUS | MAP_FIXED, -1, 0));
    ASSERT_EQ(addr, program_break + (5 * page_size));

    // Fill the writable `mmap()`ed pages with identifiable content.
    memset(break_memory + page_size, 'b', page_size);
    memset(break_memory + 4 * page_size, 'c', page_size);

    // Extend the program break by another eight pages.
    new_break = brk_syscall(program_break + (16 * page_size));
    ASSERT_EQ(new_break, program_break + (16 * page_size));

    // Everything preceding the old break should be preserved.
    EXPECT_TRUE(mem_contains(break_memory, page_size, 'a'));  // first of the old break pages
    EXPECT_TRUE(mem_contains(break_memory + page_size, page_size, 'b'));
    EXPECT_TRUE(mem_contains(break_memory + 4 * page_size, page_size, 'c'));
    EXPECT_TRUE(mem_contains(break_memory + 7 * page_size, page_size,
                             'a'));  // last of the old break pages.

    // The new break area will be zeroed.
    EXPECT_TRUE(mem_contains(break_memory + 8 * page_size, 8 * page_size, 0));

    _exit(testing::Test::HasFailure());
  });
}

// Verify that the program break will not grow over existing private mappings.
TEST(Task, BrkWillNotGrowOverPrivateAnonMmap) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([&] {
    ASSERT_NE(set_brk_page_aligned(), 0u);

    size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));

    // Create anonymous mapped pages after the break.
    uintptr_t program_break = brk_syscall(0);

    // Anonymously map over second and third pages, with RW and read-only private pages.
    uintptr_t addr = reinterpret_cast<uintptr_t>(
        mmap(reinterpret_cast<void*>(program_break + page_size), page_size, PROT_WRITE | PROT_READ,
             MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0));
    ASSERT_EQ(addr, program_break + page_size);
    addr = reinterpret_cast<uintptr_t>(
        mmap(reinterpret_cast<void*>(program_break + (2 * page_size)), page_size, PROT_READ,
             MAP_PRIVATE | MAP_ANONYMOUS | MAP_FIXED, -1, 0));
    ASSERT_EQ(addr, program_break + (2 * page_size));

    // Attempt to grow the program break over the private mappings.
    // This should fail, causing the old program break position to remain current.
    uintptr_t new_break = brk_syscall(program_break + (8 * page_size));
    EXPECT_EQ(new_break, program_break);

    _exit(testing::Test::HasFailure());
  });
}

// Verify that the program break will not grow over existing private mappings.
TEST(Task, BrkWillNotGrowOverSharedAnonMmap) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([&] {
    ASSERT_NE(set_brk_page_aligned(), 0u);

    size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));

    // Create shared mapped pages after the break.
    uintptr_t program_break = brk_syscall(0);

    // Anonymously map over second and fourth pages, with RW and read-only shared pages.
    uintptr_t addr = reinterpret_cast<uintptr_t>(
        mmap(reinterpret_cast<void*>(program_break + (page_size)), page_size,
             PROT_WRITE | PROT_READ, MAP_SHARED | MAP_ANONYMOUS | MAP_FIXED, -1, 0));
    ASSERT_EQ(addr, program_break + page_size);
    addr = reinterpret_cast<uintptr_t>(
        mmap(reinterpret_cast<void*>(program_break + (3 * page_size)), page_size, PROT_READ,
             MAP_SHARED | MAP_ANONYMOUS | MAP_FIXED, -1, 0));
    ASSERT_EQ(addr, program_break + (3 * page_size));

    // Attempt to grow the program break over the shared mappings.
    // This should fail, causing the old program break position to remain current.
    uintptr_t new_break = brk_syscall(program_break + (8 * page_size));
    EXPECT_EQ(new_break, program_break);

    _exit(testing::Test::HasFailure());
  });
}

// Verify that the `brk()` syscall continues to function after pages within the
// program break have been explicitly unmapped by the caller.
TEST(Task, BrkCanBeUnmapped) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([&] {
    ASSERT_NE(set_brk_page_aligned(), 0u);

    size_t page_size = SAFE_SYSCALL(sysconf(_SC_PAGE_SIZE));

    // Allocate a page from the program break.
    uintptr_t program_break = brk_syscall(0);
    uintptr_t new_break = brk_syscall(program_break + page_size);

    // Unmap the last page we just added.
    ASSERT_TRUE(test_helper::TryRead(program_break));
    SAFE_SYSCALL(munmap(reinterpret_cast<void*>(program_break), page_size));
    EXPECT_FALSE(test_helper::TryRead(program_break));

    // The program break hasn't changed.
    uintptr_t break_after_unmap = brk_syscall(0);
    EXPECT_EQ(break_after_unmap, new_break);

    // The program break can still be extended.
    uintptr_t final_break = brk_syscall(new_break + page_size);
    EXPECT_EQ(final_break, new_break + page_size);

    // The final page of the break is readable, and the unmapped page is still not.
    EXPECT_TRUE(test_helper::TryRead(new_break));
    EXPECT_FALSE(test_helper::TryRead(program_break));

    // The break can be rewound over the unmapped region.
    EXPECT_EQ(brk_syscall(program_break), program_break);

    _exit(testing::Test::HasFailure());
  });

  EXPECT_TRUE(helper.WaitForChildren());
}

// Verify that the current program break address is treated as outside the break.
TEST(Task, BrkIsOutOfRange) {
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([] {
    // Set the program break to be page-aligned.
    ASSERT_NE(set_brk_page_aligned(), 0u);

    // The break address should refer to the first byte of the page after the break,
    // and therefore not be mapped.
    uintptr_t aligned_break = brk_syscall(0);
    EXPECT_FALSE(test_helper::TryRead(aligned_break));

    // Increasing the break by one byte effectively "allocates" the byte at the
    // aligned break address, causing that page to now be mapped.
    uintptr_t increased_break = aligned_break + 1;
    EXPECT_EQ(brk_syscall(increased_break), increased_break);
    EXPECT_TRUE(test_helper::TryRead(aligned_break));

    // Reducing the break by one byte "deallocates" the byte at the aligned
    // address, causing the page to be unmapped.
    EXPECT_EQ(brk_syscall(aligned_break), aligned_break);
    EXPECT_FALSE(test_helper::TryRead(aligned_break));

    _exit(testing::Test::HasFailure());
  });

  EXPECT_TRUE(helper.WaitForChildren());
}

TEST(Task, ChildCantModifyParent) {
  ASSERT_GT(MAX_PAGE_ALIGNMENT, getpagesize());

  test_helper::ForkHelper helper;

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

  test_helper::ForkHelper helper;

  g_global_variable_data = 1;
  g_global_variable_bss = 10;
  volatile int local_variable = 100;
  volatile int* heap_variable = new volatile int();
  *heap_variable = 1000;

  ASSERT_EQ(g_global_variable_data, 1u);
  ASSERT_EQ(g_global_variable_bss, 10);
  ASSERT_EQ(local_variable, 100);
  ASSERT_EQ(*heap_variable, 1000);

  test_helper::SignalMaskHelper signal_helper = test_helper::SignalMaskHelper();
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
  test_helper::ForkHelper helper;
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
  test_helper::ForkHelper helper;
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
  test_helper::ForkHelper helper;
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
  test_helper::ForkHelper helper;
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

TEST(Task, ForkPreservesStackStart) {
  pid_t parent_pid = getpid();
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([parent_pid] {
    uintptr_t parent_stack_start, child_stack_start;
    ASSERT_NO_FATAL_FAILURE(ReadStackStart(parent_pid, &parent_stack_start));
    ASSERT_NO_FATAL_FAILURE(ReadStackStart(getpid(), &child_stack_start));
    EXPECT_EQ(parent_stack_start, child_stack_start);
  });
  ASSERT_TRUE(helper.WaitForChildren());
}

TEST(Task, ForkPreservesStackSize) {
  pid_t parent_pid = getpid();
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([parent_pid] {
    uintptr_t parent_stack_size, child_stack_size;
    ASSERT_NO_FATAL_FAILURE(ReadStackSize(parent_pid, &parent_stack_size));
    ASSERT_NO_FATAL_FAILURE(ReadStackSize(getpid(), &child_stack_size));
    EXPECT_EQ(parent_stack_size, child_stack_size);
  });
  ASSERT_TRUE(helper.WaitForChildren());
}

TEST(Task, ForkPreservesAux) {
  pid_t parent_pid = getpid();
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([parent_pid] {
    std::vector<uint8_t> parent_auxv, child_auxv;
    ASSERT_NO_FATAL_FAILURE(ReadProcPidFile(parent_pid, "auxv", &parent_auxv));
    ASSERT_NO_FATAL_FAILURE(ReadProcPidFile(getpid(), "auxv", &child_auxv));
    EXPECT_EQ(parent_auxv, child_auxv);
  });
  ASSERT_TRUE(helper.WaitForChildren());
}

TEST(Task, ForkPreservesCmdline) {
  pid_t parent_pid = getpid();
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([parent_pid] {
    std::vector<uint8_t> parent_cmdline, child_cmdline;
    ASSERT_NO_FATAL_FAILURE(ReadProcPidFile(parent_pid, "cmdline", &parent_cmdline));
    ASSERT_NO_FATAL_FAILURE(ReadProcPidFile(getpid(), "cmdline", &child_cmdline));
    EXPECT_EQ(parent_cmdline, child_cmdline);
  });
  ASSERT_TRUE(helper.WaitForChildren());
}

TEST(Task, ForkPreservesEnviron) {
  pid_t parent_pid = getpid();
  test_helper::ForkHelper helper;
  helper.RunInForkedProcess([parent_pid] {
    std::vector<uint8_t> parent_environ, child_environ;
    ASSERT_NO_FATAL_FAILURE(ReadProcPidFile(parent_pid, "environ", &parent_environ));
    ASSERT_NO_FATAL_FAILURE(ReadProcPidFile(getpid(), "environ", &child_environ));
    EXPECT_EQ(parent_environ, child_environ);
  });
  ASSERT_TRUE(helper.WaitForChildren());
}
