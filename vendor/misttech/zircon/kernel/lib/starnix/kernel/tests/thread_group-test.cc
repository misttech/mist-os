// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/task/thread_group.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {

bool test_setsid() {
  BEGIN_TEST;
  // TODO (Herrera)
  END_TEST;
}
}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_thread_group)
UNITTEST("test setsid", unit_testing::test_setsid)
UNITTEST_END_TESTCASE(starnix_thread_group, "starnix_thread_group", "Tests for Thread Group")
