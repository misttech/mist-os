// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sched.h>
#include <signal.h>
#include <stdint.h>
#include <stdio.h>
#include <stdlib.h>
#include <sys/mman.h>
#include <unistd.h>

#include <climits>
#include <csignal>
#include <cstdint>

#include <gtest/gtest.h>

namespace {

uintptr_t stack_ptr;

constexpr int kSigaltstackFailed = -1;
constexpr int kHandlerOnSigaltStack = 1;
constexpr int kHandlerOnMainStack = 2;

void handler(int sig) { stack_ptr = reinterpret_cast<uintptr_t>(__builtin_frame_address(0)); }

void overflow_handler(int sig) {
  stack_t current_altstack;
  int ret = sigaltstack(nullptr, &current_altstack);
  if (ret < 0) {
    exit(kSigaltstackFailed);
  }
  if (current_altstack.ss_flags == SA_ONSTACK) {
    exit(kHandlerOnSigaltStack);
  } else {
    exit(kHandlerOnMainStack);
  }
}

int setup_overflow_altstack(void *alt_stack) {
  stack_t ss = {
      .ss_sp = alt_stack,
      .ss_size = UINT64_MAX,
  };
  int ret = sigaltstack(&ss, nullptr);
  if (ret < 0) {
    return ret;
  }

  struct sigaction sa = {};
  sa.sa_handler = overflow_handler;
  sa.sa_flags = SA_ONSTACK;
  ret = sigaction(SIGUSR1, &sa, nullptr);
  if (ret < 0) {
    return ret;
  }

  ret = raise(SIGUSR1);
  if (ret != 0) {
    return ret;
  }
  return 0;
}

TEST(SignalHandling, UseSigaltstackSucceeds) {
  constexpr size_t kStackSize = 0x20000;
  void *stack = mmap(nullptr, kStackSize, PROT_WRITE | PROT_READ,
                     MAP_STACK | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  ASSERT_NE(MAP_FAILED, stack);

  stack_t ss = {
      .ss_sp = stack,
      .ss_size = kStackSize,
  };
  ASSERT_EQ(0, sigaltstack(&ss, nullptr));

  struct sigaction sa = {};
  sa.sa_handler = handler;
  sa.sa_flags = SA_ONSTACK;
  ASSERT_EQ(0, sigaction(SIGUSR1, &sa, nullptr));
  stack_ptr = 0;
  ASSERT_NE(-1, raise(SIGUSR1));
  // Check that the sigaltstack is used.
  uintptr_t alt_stack_base = reinterpret_cast<uintptr_t>(stack);
  EXPECT_GE(stack_ptr, alt_stack_base);
  EXPECT_LE(stack_ptr, alt_stack_base + kStackSize);

  // Cleanup
  ss = {
      .ss_flags = SS_DISABLE,
  };
  ASSERT_EQ(0, sigaltstack(&ss, nullptr));
}

// Check that if the stack pointer calculation for the sigaltstack overflows,
// the main stack is used instead.
TEST(SignalHandling, UseMainStackOnSigaltstackOverflow) {
  constexpr size_t kStackSize = 0x20000;
  void *mmap1 = mmap(nullptr, kStackSize, PROT_WRITE | PROT_READ,
                     MAP_STACK | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  ASSERT_NE(MAP_FAILED, mmap1);
  void *mmap2 = mmap(nullptr, kStackSize, PROT_WRITE | PROT_READ,
                     MAP_STACK | MAP_PRIVATE | MAP_ANONYMOUS, -1, 0);
  ASSERT_NE(MAP_FAILED, mmap2);
  void *main_stack = std::min(mmap1, mmap2);
  void *alt_stack = std::max(mmap1, mmap2);

  uintptr_t main_stack_base = reinterpret_cast<uint64_t>(main_stack) + kStackSize;
  int pid =
      clone(setup_overflow_altstack, (void *)main_stack_base, CLONE_VFORK | SIGCHLD, alt_stack);
  ASSERT_NE(-1, pid);
  int status = 0;
  pid_t wait_pid = waitpid(pid, &status, 0);
  ASSERT_NE(-1, wait_pid);
  EXPECT_EQ(kHandlerOnMainStack, WEXITSTATUS(status));
}

TEST(SignalHandlingDeathTest, ExitsKilledBySignal) {
  std::vector<int> term_signals = {SIGABRT, SIGALRM, SIGBUS,  SIGFPE,  SIGHUP,  SIGILL,
                                   SIGINT,  SIGPIPE, SIGPOLL, SIGPROF, SIGQUIT, SIGSEGV,
                                   SIGSYS,  SIGTERM, SIGTRAP, SIGUSR1, SIGUSR2};

  for (const auto signal : term_signals) {
    EXPECT_EXIT(
        [signal]() {
          // Reset the action for this signal, the default should cause the
          // program to terminate.
          struct sigaction sa = {};
          sa.sa_handler = SIG_DFL;
          if (sigaction(signal, &sa, NULL)) {
            perror("sigaction");
            exit(EXIT_FAILURE);
          }
          raise(signal);
        }(),
        testing::KilledBySignal(signal), "");
  }

  // SIGKILL cannot be handled, so sigaction would fail.
  EXPECT_EXIT([]() { raise(SIGKILL); }(), testing::KilledBySignal(SIGKILL), "");

  // Generate signals by causing architectural exceptions.
  EXPECT_EXIT([]() { __builtin_trap(); }(), testing::KilledBySignal(SIGILL), "");
  EXPECT_EXIT([]() { __builtin_debugtrap(); }(), testing::KilledBySignal(SIGTRAP), "");

  EXPECT_EXIT(
      []() {
        // Write to a non-writable memory address to cause a SIGSEGV.
        void *res = mmap(NULL, 0x1000, PROT_NONE, MAP_ANONYMOUS | MAP_PRIVATE, -1, 0);
        if (res == MAP_FAILED) {
          exit(EXIT_FAILURE);
        }
        volatile uint8_t *buf = reinterpret_cast<volatile uint8_t *>(res);
        buf[0] = 0x1;  // Page Fault.
      }(),
      testing::KilledBySignal(SIGSEGV), "");
}

}  // namespace
