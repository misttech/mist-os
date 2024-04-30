// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/testing/testing.h>

#include <fbl/ref_ptr.h>
#include <zxtest/zxtest.h>

namespace {

TEST(Task, test_tid_allocation) {
  auto [kernel, current_task] = starnix::testing::create_kernel_and_task();

  ASSERT_EQ(1, current_task->get_tid());

  auto another_current = starnix::testing::create_task(kernel, "another-task");
  pid_t another_tid = another_current->get_tid();

  ASSERT_GE(2, another_tid);

  {
    Guard<Mutex> guard{kernel->pidtable_rw_lock()};
    ASSERT_EQ(1, kernel->pids().get_task(1)->get_tid());
    ASSERT_EQ(another_tid, kernel->pids().get_task(another_tid)->get_tid());
    ASSERT_NULL(kernel->pids().get_task(another_tid + 1));
  }
}

}  // namespace
