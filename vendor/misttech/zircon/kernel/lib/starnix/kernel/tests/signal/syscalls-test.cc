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
#include <lib/unittest/unittest.h>

#include <memory>

#include <fbl/ref_ptr.h>
#include <ktl/string_view.h>

#include <linux/prctl.h>

using namespace starnix::testing;

namespace unit_testing {

using starnix::ExitStatus;
using starnix::ProcessExitInfo;
using starnix::ProcessSelector;
using starnix::WaitingOptions;
using starnix::WaitResult;

bool test_no_error_when_zombie() {
  BEGIN_TEST;
  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto child = (*current_task).clone_task_for_test(0, starnix_uapi::kSIGCHLD);

  auto expected_result = WaitResult{
      .pid = (*child)->id(),
      .uid = 0,
      .exit_info =
          ProcessExitInfo{
              .status = ExitStatus::Exit(1),
              .exit_signal = starnix_uapi::kSIGCHLD,
          },
      .time_stats = {},
  };

  (*child)->thread_group()->exit(ExitStatus::Exit(1), ktl::nullopt);
  child.~AutoReleasableTask();

  auto result = friend_wait_on_pid(*current_task, ProcessSelector::AnyProcess(),
                                   WaitingOptions::new_for_wait4(0, 0).value());
  ASSERT_TRUE(result.is_ok());
  ASSERT_TRUE(result.value().has_value());

  ASSERT_EQ(expected_result.pid, result.value()->pid);
  ASSERT_EQ(expected_result.uid, result.value()->uid);
  ASSERT_EQ(ExitStatus::wait_status(expected_result.exit_info.status),
            ExitStatus::wait_status(result.value()->exit_info.status));
  ASSERT_EQ(expected_result.time_stats.user_time_ns, result.value()->time_stats.user_time_ns);
  ASSERT_EQ(expected_result.time_stats.system_time_ns, result.value()->time_stats.system_time_ns);

  END_TEST;
}

bool test_wait4_by_pgid() {
  BEGIN_TEST;

  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto child1 = (*current_task).clone_task_for_test(0, starnix_uapi::kSIGCHLD);
  auto child1_pid = (*child1)->id();
  (*child1)->thread_group()->exit(starnix::ExitStatus::Exit(42), ktl::nullopt);
  child1.~AutoReleasableTask();
  auto child2 = (*current_task).clone_task_for_test(0, starnix_uapi::kSIGCHLD);
  ASSERT_TRUE((*child2)->thread_group()->setsid().is_ok(), "setsid");
  auto child2_pid = (*child2)->id();
  (*child2)->thread_group()->exit(starnix::ExitStatus::Exit(42), ktl::nullopt);
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
}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_signal_syscalls)
UNITTEST("test no error when zombie", unit_testing::test_no_error_when_zombie)
UNITTEST("test wait4 by pgid", unit_testing::test_wait4_by_pgid)
UNITTEST_END_TESTCASE(starnix_signal_syscalls, "starnix_signal_syscalls",
                      "Tests for Signal Syscalls")
