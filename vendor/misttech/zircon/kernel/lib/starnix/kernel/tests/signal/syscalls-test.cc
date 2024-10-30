// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/signals/syscalls.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/mistos/util/default_construct.h>
#include <lib/mistos/util/testing/unittest.h>
#include <lib/mistos/util/weak_wrapper.h>
#include <lib/unittest/unittest.h>
#include <zircon/assert.h>

#include <fbl/ref_ptr.h>
#include <ktl/string_view.h>

#include <linux/prctl.h>

namespace unit_testing {

namespace {

using starnix::ExitStatus;
using starnix::ProcessExitInfo;
using starnix::ProcessSelector;
using starnix::WaitingOptions;
using starnix::WaitResult;
using starnix::testing::AutoReleasableTask;

/// Wait4 does not support all options.
bool test_wait4_options() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  auto id = 1;

  ASSERT_EQ(sys_wait4(*current_task, id, mtl::DefaultConstruct<starnix_uapi::UserRef<int32_t>>(),
                      WEXITED, mtl::DefaultConstruct<starnix_uapi::UserRef<struct ::rusage>>())
                .error_value()
                .error_code(),
            errno(EINVAL).error_code());
  ASSERT_EQ(sys_wait4(*current_task, id, mtl::DefaultConstruct<starnix_uapi::UserRef<int32_t>>(),
                      WNOWAIT, mtl::DefaultConstruct<starnix_uapi::UserRef<struct ::rusage>>())
                .error_value()
                .error_code(),
            errno(EINVAL).error_code());
  ASSERT_EQ(sys_wait4(*current_task, id, mtl::DefaultConstruct<starnix_uapi::UserRef<int32_t>>(),
                      0xffff, mtl::DefaultConstruct<starnix_uapi::UserRef<struct ::rusage>>())
                .error_value()
                .error_code(),
            errno(EINVAL).error_code());
  END_TEST;
}

bool test_echild_when_no_zombie() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();

  // Send the signal to the task.
  ASSERT_TRUE(
      sys_kill(*current_task, (*current_task)->get_pid(), UncheckedSignal::From(SIGCHLD)).is_ok());

  // Verify that ECHILD is returned because there is no zombie process and no children to block
  // waiting for.
  auto result = friend_wait_on_pid(*current_task, ProcessSelector::AnyProcess(),
                                   WaitingOptions::new_for_wait4(0, 0).value());
  ASSERT_TRUE(result.is_error());
  ASSERT_EQ(result.error_value().error_code(), errno(ECHILD).error_code());

  END_TEST;
}

bool test_no_error_when_zombie() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  auto child = (*current_task).clone_task_for_test(0, starnix_uapi::kSIGCHLD);

  auto expected_result = WaitResult{
      .pid = (*child)->id_,
      .uid = 0,
      .exit_info =
          ProcessExitInfo{
              .status = ExitStatus::Exit(1),
              .exit_signal = starnix_uapi::kSIGCHLD,
          },
      .time_stats = {},
  };

  (*child)->thread_group_->exit(ExitStatus::Exit(1), ktl::nullopt);
  child.~AutoReleasableTask();

  auto result = friend_wait_on_pid(*current_task, ProcessSelector::AnyProcess(),
                                   WaitingOptions::new_for_wait4(0, 0).value());
  ASSERT_TRUE(result.is_ok());
  ASSERT_TRUE(result.value().has_value());

  ASSERT_EQ(expected_result.pid, result.value()->pid);
  ASSERT_EQ(expected_result.uid, result.value()->uid);
  ASSERT_EQ(expected_result.exit_info.status.wait_status(),
            result.value()->exit_info.status.wait_status());
  ASSERT_EQ(expected_result.time_stats.user_time_ns, result.value()->time_stats.user_time_ns);
  ASSERT_EQ(expected_result.time_stats.system_time_ns, result.value()->time_stats.system_time_ns);

  END_TEST;
}

struct thread_args {
  util::WeakPtr<starnix::Task> task;
  starnix::TaskBuilder task_builder;
};

bool test_waiting_for_child() {
  BEGIN_TEST;
  auto [kernel, task] = starnix::testing::create_kernel_task_and_unlocked();

  auto child =
      (*task).clone_task(0, starnix_uapi::kSIGCHLD, mtl::DefaultConstruct<UserRef<pid_t>>(),
                         mtl::DefaultConstruct<UserRef<pid_t>>());
  ASSERT_TRUE(child.is_ok(), "clone_task");

  auto child_task = ktl::move(child.value());

  // No child is currently terminated.
  auto result = friend_wait_on_pid(*task, ProcessSelector::AnyProcess(),
                                   WaitingOptions::new_for_wait4(WNOHANG, 0).value());
  ASSERT_TRUE(result.is_ok());
  ASSERT_FALSE(result.value().has_value());

  thread_args args = {.task = task->weak_task(), .task_builder = ktl::move(child_task)};
  Thread* thread = Thread::Create(
      "",
      [](void* arg) -> int {
        thread_args* args = reinterpret_cast<thread_args*>(arg);
        auto tsk = args->task.Lock();
        ZX_ASSERT_MSG(tsk, "task must be alive");

        AutoReleasableTask chld = AutoReleasableTask::From(ktl::move(args->task_builder));

        // Wait for the main thread to be blocked on waiting for a child.
        while (!tsk->Read()->is_blocked()) {
          Thread::Current::SleepRelative(ZX_MSEC(10));
        }
        (*chld)->thread_group_->exit(ExitStatus::Exit(0), ktl::nullopt);
        return (*chld)->id_;
      },
      &args, DEFAULT_PRIORITY);

  thread->Resume();

  // Block until child is terminated.
  result = friend_wait_on_pid(*task, ProcessSelector::AnyProcess(),
                              WaitingOptions::new_for_wait4(0, 0).value());
  ASSERT_TRUE(result.is_ok());
  ASSERT_TRUE(result.value().has_value());
  auto waited_child = result.value().value();

  // Child is deleted, the thread must be able to terminate.
  int child_id = 0;
  thread->Join(&child_id, ZX_TIME_INFINITE);
  ASSERT_EQ(child_id, waited_child.pid);

  END_TEST;
}

bool test_wait4_by_pgid() {
  BEGIN_TEST;

  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  auto child1 = (*current_task).clone_task_for_test(0, starnix_uapi::kSIGCHLD);
  auto child1_pid = (*child1)->id_;
  (*child1)->thread_group_->exit(starnix::ExitStatus::Exit(42), ktl::nullopt);
  child1.~AutoReleasableTask();
  auto child2 = (*current_task).clone_task_for_test(0, starnix_uapi::kSIGCHLD);
  ASSERT_TRUE((*child2)->thread_group_->setsid().is_ok(), "setsid");
  auto child2_pid = (*child2)->id_;
  (*child2)->thread_group_->exit(starnix::ExitStatus::Exit(42), ktl::nullopt);
  child2.~AutoReleasableTask();

  auto result = sys_wait4(*current_task, -child2_pid, mtl::DefaultConstruct<UserRef<int32_t>>(), 0,
                          mtl::DefaultConstruct<UserRef<struct ::rusage>>());
  ASSERT_TRUE(result.is_ok());
  ASSERT_EQ(child2_pid, result.value());

  result = sys_wait4(*current_task, 0, mtl::DefaultConstruct<UserRef<int32_t>>(), 0,
                     mtl::DefaultConstruct<UserRef<struct ::rusage>>());
  ASSERT_TRUE(result.is_ok());
  ASSERT_EQ(child1_pid, result.value());
  END_TEST;
}

}  // namespace

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_signal_syscalls)
UNITTEST("test wait4 options", unit_testing::test_wait4_options)
UNITTEST("test echild when no zombie", unit_testing::test_echild_when_no_zombie)
UNITTEST("test no error when zombie", unit_testing::test_no_error_when_zombie)
UNITTEST("test waiting for child", unit_testing::test_waiting_for_child)
UNITTEST("test wait4 by pgid", unit_testing::test_wait4_by_pgid)
UNITTEST_END_TESTCASE(starnix_signal_syscalls, "starnix_signal_syscalls",
                      "Tests for Signal Syscalls")
