// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/testing/testing.h>
#include <lib/mistos/starnix_uapi/resource_limits.h>
#include <lib/mistos/starnix_uapi/signals.h>

#include <fbl/ref_ptr.h>
#include <zxtest/zxtest.h>

#include <linux/sched.h>

using namespace starnix::testing;

namespace {

TEST(Task, test_tid_allocation) {
  auto [kernel, current_task] = create_kernel_and_task();

  ASSERT_EQ(1, current_task->get_tid());

  auto another_current = create_task(kernel, "another-task");
  pid_t another_tid = another_current->get_tid();

  ASSERT_GE(2, another_tid);

  auto pids = kernel->pids.Read();
  ASSERT_EQ(1, pids->get_task(1).Lock()->get_tid());
  ASSERT_EQ(another_tid, pids->get_task(another_tid).Lock()->get_tid());
}

TEST(Task, test_clone_pid_and_parent_pid) {
  auto [kernel, current_task] = create_kernel_and_task();
  auto thread =
      (*current_task)
          .clone_task_for_test(static_cast<uint64_t>(CLONE_THREAD | CLONE_VM | CLONE_SIGHAND),
                               starnix_uapi::kSIGCHLD);
  ASSERT_EQ(current_task->get_pid(), thread->get_pid());
  ASSERT_NE(current_task->get_tid(), thread->get_tid());
  ASSERT_EQ(current_task->thread_group->leader, thread->thread_group->leader);

  auto child_task = (*current_task).clone_task_for_test(0, starnix_uapi::kSIGCHLD);

  ASSERT_NE(current_task->get_pid(), child_task->get_pid());
  ASSERT_NE(current_task->get_tid(), child_task->get_tid());
  ASSERT_EQ(current_task->get_pid(), child_task->thread_group->read().get_ppid());
}

TEST(Task, DISABLED_test_root_capabilities) {
  auto [kernel, current_task] = create_kernel_and_task();
  // ASSERT_TRUE( (*current_task)->creds().)
}

TEST(Task, test_clone_rlimit) {
  auto [kernel, current_task] = create_kernel_task_and_unlocked();
  auto prev_fsize = (*current_task)->thread_group->get_rlimit({starnix_uapi::ResourceEnum::FSIZE});
  ASSERT_NE(10, prev_fsize);
  (*current_task)->thread_group->limits.Lock()->set({starnix_uapi::ResourceEnum::FSIZE}, {10, 100});
  auto current_fsize =
      (*current_task)->thread_group->get_rlimit({starnix_uapi::ResourceEnum::FSIZE});
  ASSERT_EQ(10, current_fsize);

  auto child_task = (*current_task).clone_task_for_test(0, starnix_uapi::kSIGCHLD);
  auto child_fsize = (*child_task)->thread_group->get_rlimit({starnix_uapi::ResourceEnum::FSIZE});
  ASSERT_EQ(10, child_fsize);
}

}  // namespace
