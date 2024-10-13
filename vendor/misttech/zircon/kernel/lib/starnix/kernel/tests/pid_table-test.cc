// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/task/pid_table.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {

using namespace starnix;

bool pid_table_allocate_pid() {
  BEGIN_TEST;

  PidTable table;
  ASSERT_EQ(1, table.allocate_pid());

  END_TEST;
}

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_pid_table)
UNITTEST("test trivial pid allocation", unit_testing::pid_table_allocate_pid)
UNITTEST_END_TESTCASE(starnix_pid_table, "starnix_pid_table", "Tests for pid")
