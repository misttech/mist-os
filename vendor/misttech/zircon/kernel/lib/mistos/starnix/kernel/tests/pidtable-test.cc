// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/task/pidtable.h>
#include <lib/mistos/starnix/kernel/task/process_group.h>
#include <lib/mistos/starnix/kernel/task/task.h>
#include <lib/mistos/starnix/kernel/task/thread_group.h>

#include <zxtest/zxtest.h>

namespace starnix {

TEST(PidTable, AllocatePid) {
  PidTable table;
  ASSERT_EQ(1, table.allocate_pid());
}

}  // namespace starnix
