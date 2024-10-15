// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/task/current_task.h>
#include <lib/mistos/starnix/kernel/task/exit_status.h>
#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/mistos/starnix_uapi/signals.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {

namespace {

bool test_setsid() {
  BEGIN_TEST;
  // TODO (Herrera)
  END_TEST;
}

bool test_exit_status() {
  BEGIN_TEST;
  auto [kernel, current_task] = starnix::testing::create_kernel_task_and_unlocked();
  {
    auto child = (*current_task).clone_task_for_test(0, {kSIGCHLD});
    child->thread_group()->exit(starnix::ExitStatusExit(42), {});
  }
  // ASSERT_EQ(current_task->thread_group->read(), starnix::ExitStatusExit(42))

  END_TEST;
}

}  // namespace

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_thread_group)
UNITTEST("test setsid", unit_testing::test_setsid)
UNITTEST("test exit status", unit_testing::test_exit_status)
UNITTEST_END_TESTCASE(starnix_thread_group, "starnix_thread_group", "Tests for Thread Group")
