// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/task/kernel.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/syscalls.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/mistos/starnix/testing/testing.h>

#include <fbl/ref_ptr.h>
#include <zxtest/zxtest.h>

using namespace starnix::testing;

namespace {

TEST(Task, test_sys_getsid) {
  auto [kernel, current_task] = create_kernel_and_task();

  auto result = starnix::sys_getsid(*current_task, 0);
  ASSERT_FALSE(result.is_error(), "failed to get sid");

  ASSERT_EQ(current_task->get_tid(), result.value());

  auto second_current = create_task(kernel, "second task");

  result = starnix::sys_getsid(*current_task, second_current->get_tid());
  ASSERT_FALSE(result.is_error(), "failed to get sid");

  ASSERT_EQ(second_current->get_tid(), result.value());
}

}  // namespace
