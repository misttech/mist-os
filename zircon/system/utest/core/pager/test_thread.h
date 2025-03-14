// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSTEM_UTEST_CORE_PAGER_TEST_THREAD_H_
#define ZIRCON_SYSTEM_UTEST_CORE_PAGER_TEST_THREAD_H_

#include <lib/fit/function.h>
#include <lib/sync/completion.h>
#include <lib/zx/channel.h>
#include <lib/zx/suspend_token.h>
#include <lib/zx/thread.h>
#include <threads.h>
#include <zircon/syscalls/exception.h>
#include <zircon/types.h>

#include <zxtest/zxtest.h>

namespace pager_tests {

// Class which executes the specified function on a test thread.
class TestThread {
 public:
  explicit TestThread(fit::function<bool()> fn);
  ~TestThread();

  // Starts the test thread's execution.
  bool Start();
  // Block until the test thread successfully terminates.
  bool Wait();
  // Block until the test thread terminates with a validation error.
  bool WaitForFailure();
  // Block until the test thread crashes due to an access to |crash_addr|. The exception report's
  // |synth_code| field should be set to |error_status|.
  bool WaitForCrash(uintptr_t crash_addr, zx_status_t error_status);
  // Block until the test thread crashes for any reason.
  bool WaitForAnyCrash();

  // Block until the test thread is blocked against a pager.
  // NOTE: It is only valid to call this in an environment where there is no interference from an
  // external pager. More specifically, if the test is meant to be run as a component, the caller
  // cannot rely on a true return value since the thread might be blocked against a system pager.
  bool WaitForBlocked() { return WaitForBlockedOnPager(zx_thread_); }
  // Block until the thread terminates.
  bool WaitForTerm() {
    return zx_thread_.wait_one(ZX_TASK_TERMINATED, zx::time::infinite(), nullptr) == ZX_OK;
  }
  // Wait until the thread suspends.
  bool WaitForSuspend() {
    return zx_thread_.wait_one(ZX_THREAD_SUSPENDED, zx::time::infinite(), nullptr) == ZX_OK;
  }

  // Issue a suspend without waiting for it to complete.
  void Suspend() { ASSERT_EQ(zx_thread_.suspend(&suspend_token_), ZX_OK); }
  // Issue a suspend and wait until the thread successfully suspends.
  void SuspendSync() {
    ASSERT_EQ(zx_thread_.suspend(&suspend_token_), ZX_OK);

    zx_signals_t observed = 0u;
    ASSERT_EQ(zx_thread_.wait_one(ZX_THREAD_SUSPENDED, zx::time::infinite(), &observed), ZX_OK);
  }

  void Resume() { suspend_token_.reset(); }

  void Run();

  static bool WaitForBlockedOnPager(zx::thread& thread);

 private:
  enum class ExpectStatus {
    Success,
    Failure,
    Crash,
  };
  bool Wait(ExpectStatus expect, std::optional<std::pair<uintptr_t, zx_status_t>> crash_info);
  void PrintDebugInfo(const zx_exception_report_t& report);

  const fit::function<bool()> fn_;

  thrd_t thrd_ = 0;
  zx::thread zx_thread_;
  zx::channel exception_channel_;
  bool success_ = false;

  zx::suspend_token suspend_token_;

  // Makes sure that everything is set up before starting the actual test function.
  sync_completion_t startup_sync_;
};

}  // namespace pager_tests

#endif  // ZIRCON_SYSTEM_UTEST_CORE_PAGER_TEST_THREAD_H_
