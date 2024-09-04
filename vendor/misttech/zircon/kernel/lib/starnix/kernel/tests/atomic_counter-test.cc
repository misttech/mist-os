// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/lifecycle/atomic_counter.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {

using namespace starnix;

bool test_new() {
  BEGIN_TEST;
  const AtomicCounter<uint64_t> COUNTER = AtomicCounter<uint64_t>::New(0);
  ASSERT_EQ(0u, COUNTER.get());
  END_TEST;
}

bool test_one_thread() {
  BEGIN_TEST;
  auto counter = AtomicCounter<uint64_t>();
  ASSERT_EQ(0u, counter.get());
  ASSERT_EQ(0u, counter.add(5));
  ASSERT_EQ(5u, counter.get());
  ASSERT_EQ(5u, counter.next());
  ASSERT_EQ(6u, counter.get());
  counter.reset(2);
  ASSERT_EQ(2u, counter.get());
  ASSERT_EQ(2u, counter.next());
  ASSERT_EQ(3u, counter.get());
  END_TEST;
}

}  // namespace unit_testing

UNITTEST_START_TESTCASE(starnix_atomic_counter)
UNITTEST("test new", unit_testing::test_new)
UNITTEST("test one thread", unit_testing::test_one_thread)
UNITTEST_END_TESTCASE(starnix_atomic_counter, "starnix_atomic_counter", "Tests for Atomic Counter")
