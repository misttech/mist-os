// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <signal.h>
#include <stdint.h>
#include <stdlib.h>
#include <sys/wait.h>
#include <unistd.h>

#include <atomic>
#include <vector>

#include <gtest/gtest.h>

#include "src/starnix/tests/syscalls/cpp/test_helper.h"

namespace {

std::vector<int> g_received_signals;
std::atomic<int> g_bad_signal_code;

void handler(int signum, siginfo_t *info, void *ucontext) {
  if (info->si_code != CLD_EXITED) {
    g_bad_signal_code.store(info->si_code);
    return;
  }

  g_received_signals.push_back(signum);
}

// This test_helper::CloneHelper instance must only be used after a clone without 'CLONE_THREAD |
// CLONE_VM'.
test_helper::CloneHelper nested_clone_helper;

void ensureWait(int pid, unsigned int waitFlags) {
  int actual_waitpid = waitpid(pid, NULL, waitFlags);
  EXPECT_EQ(errno, 0);
  EXPECT_EQ(pid, actual_waitpid);
}
}  // namespace

class SignalHelper {
 public:
  SignalHelper() {
    g_received_signals.clear();
    g_received_signals.reserve(10);
    g_bad_signal_code.store(0);
    errno = 0;

    struct sigaction sa;
    sa.sa_sigaction = handler;
    sa.sa_flags = SA_SIGINFO | SA_RESTART | SA_NOCLDSTOP;

    sigaction(SIGUSR1, &sa, &old_usr1_act_);
    sigaction(SIGCHLD, &sa, &old_chld_act_);
  }

  ~SignalHelper() {
    sigaction(SIGUSR1, &old_usr1_act_, nullptr);
    sigaction(SIGCHLD, &old_chld_act_, nullptr);
  }

  SignalHelper(const SignalHelper &) = delete;
  SignalHelper &operator=(const SignalHelper &) = delete;

 private:
  struct sigaction old_usr1_act_;
  struct sigaction old_chld_act_;
};

/*
 * Main process (P0) creates a child process (P1).
 * On termination, P1 sends its exit signal (if any) to P0.
 */
TEST(WaitpidExitSignalTest, childProcessSendsDefaultSignalOnTerminationToParentProcess) {
  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([] {
    SignalHelper signal_helper;
    test_helper::CloneHelper test_clone_helper;
    int pid = test_clone_helper.runInClonedChild(SIGCHLD, test_helper::CloneHelper::doNothing);
    ensureWait(pid, __WALL);
    EXPECT_TRUE(g_received_signals.size() == 1);
    EXPECT_EQ(g_received_signals[0], SIGCHLD);
    EXPECT_EQ(0, g_bad_signal_code.load());
  });

  EXPECT_TRUE(helper.WaitForChildren());
}

TEST(WaitpidExitSignalTest, childProcessSendsCustomExitSignalOnTerminationToParentProcess) {
  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([] {
    SignalHelper signal_helper;
    test_helper::CloneHelper test_clone_helper;
    int pid = test_clone_helper.runInClonedChild(SIGUSR1, test_helper::CloneHelper::doNothing);
    ensureWait(pid, __WALL);
    EXPECT_TRUE(g_received_signals.size() == 1);
    EXPECT_EQ(g_received_signals[0], SIGUSR1);
    EXPECT_EQ(0, g_bad_signal_code.load());
  });
  EXPECT_TRUE(helper.WaitForChildren());
}

TEST(WaitpidExitSignalTest, childProcessSendsNoExitSignalOnTerminationToParentProcess) {
  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([] {
    SignalHelper signal_helper;
    test_helper::CloneHelper test_clone_helper;
    int pid = test_clone_helper.runInClonedChild(0, test_helper::CloneHelper::doNothing);
    ensureWait(pid, __WALL);
    EXPECT_TRUE(g_received_signals.empty());
  });
  EXPECT_TRUE(helper.WaitForChildren());
}

/*
 * Main process (P0) creates a child process (P1) and P1 creates a child thread (T1).
 * After both P1 and T1 terminate, no matter the order of these termination, P0 should receive P1
 * exit signal.
 */
int processThatFinishAfterChildThread(void *) {
  nested_clone_helper.runInClonedChild(CLONE_THREAD | CLONE_VM | CLONE_SIGHAND | SIGUSR2,
                                       test_helper::CloneHelper::doNothing);
  test_helper::CloneHelper::sleep_1sec(NULL);
  return 0;
}

int processThatFinishBeforeChildThread(void *) {
  nested_clone_helper.runInClonedChild(CLONE_THREAD | CLONE_VM | CLONE_SIGHAND | SIGUSR2,
                                       test_helper::CloneHelper::sleep_1sec);
  return 0;
}

TEST(WaitpidExitSignalTest, childThreadGroupSendsCorrectExitSignalWhenLeaderTerminatesLast) {
  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([] {
    SignalHelper signal_helper;
    test_helper::CloneHelper test_clone_helper;
    int pid = test_clone_helper.runInClonedChild(SIGUSR1, processThatFinishAfterChildThread);
    ensureWait(pid, __WALL);
    EXPECT_TRUE(g_received_signals.size() == 1);
    EXPECT_EQ(g_received_signals[0], SIGUSR1);
    EXPECT_EQ(0, g_bad_signal_code.load());
  });
  EXPECT_TRUE(helper.WaitForChildren());
}

TEST(WaitpidExitSignalTest, childThreadGroupSendsCorrectExitSignalWhenLeaderTerminatesFirst) {
  test_helper::ForkHelper helper;

  helper.RunInForkedProcess([] {
    SignalHelper signal_helper;
    test_helper::CloneHelper test_clone_helper;
    int pid = test_clone_helper.runInClonedChild(SIGUSR1, processThatFinishBeforeChildThread);
    ensureWait(pid, __WALL);
    EXPECT_TRUE(g_received_signals.size() == 1);
    EXPECT_EQ(g_received_signals[0], SIGUSR1);
    EXPECT_EQ(0, g_bad_signal_code.load());
  });
  EXPECT_TRUE(helper.WaitForChildren());
}
