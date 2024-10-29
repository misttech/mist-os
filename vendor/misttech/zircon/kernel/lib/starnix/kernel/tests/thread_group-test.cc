// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/exit_status.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/session.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/mistos/starnix_uapi/signals.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {

namespace {

using starnix::ProcessGroup;
using starnix::Task;

bool test_setsid() {
  BEGIN_TEST;

  auto get_process_group = [](const Task& task) -> fbl::RefPtr<ProcessGroup> {
    return task.thread_group()->Read()->process_group_;
  };

  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  ASSERT_EQ(errno(EPERM).error_code(),
            (*current_task)->thread_group()->setsid().error_value().error_code());

  auto child_task = (*current_task).clone_task_for_test(0, {kSIGCHLD});
  ASSERT_TRUE(get_process_group(*(*child_task).task()) ==
              get_process_group(*(*current_task).task()));

  auto old_process_group = (*child_task)->thread_group()->Read()->process_group_;
  ASSERT_TRUE((*child_task)->thread_group()->setsid().is_ok());
  ASSERT_EQ((*child_task)->thread_group()->Read()->process_group_->session_->leader_,
            (*child_task)->get_pid());

  auto tgs = old_process_group->Read()->thread_groups();
  auto result = std::ranges::find_if(
      tgs, [&](const auto& tg) { return tg == (*child_task)->thread_group(); });
  ASSERT_TRUE(result == tgs.end());

  END_TEST;
}

bool test_exit_status() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  {
    auto child = (*current_task).clone_task_for_test(0, {kSIGCHLD});
    (*child)->thread_group()->exit(starnix::ExitStatus::Exit(42), {});
  }
  ASSERT_EQ((*current_task)
                ->thread_group()
                ->Read()
                ->zombie_children()[0]
                ->exit_info.status.signal_info_status(),
            starnix::ExitStatus::Exit(42).signal_info_status());

  END_TEST;
}

}  // namespace

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_thread_group)
UNITTEST("test setsid", unit_testing::test_setsid)
UNITTEST("test exit status", unit_testing::test_exit_status)
UNITTEST_END_TESTCASE(starnix_thread_group, "starnix_thread_group", "Tests for Thread Group")
