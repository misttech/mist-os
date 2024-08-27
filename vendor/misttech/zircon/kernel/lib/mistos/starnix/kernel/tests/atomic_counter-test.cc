// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/starnix/kernel/lifecycle/atomic_counter.h>

#include <zxtest/zxtest.h>

using namespace starnix;

namespace {

TEST(AtomicCounter, test_new) {
  const AtomicCounter<uint64_t> COUNTER = AtomicCounter<uint64_t>::New(0);
  ASSERT_EQ(0, COUNTER.get());
}

TEST(AtomicCounter, test_one_thread) {
  auto counter = AtomicCounter<uint64_t>();
  ASSERT_EQ(0, counter.get());
  ASSERT_EQ(0, counter.add(5));
  ASSERT_EQ(5, counter.get());
  ASSERT_EQ(5, counter.next());
  ASSERT_EQ(6, counter.get());
  counter.reset(2);
  ASSERT_EQ(2, counter.get());
  ASSERT_EQ(2, counter.next());
  ASSERT_EQ(3, counter.get());
}

TEST(AtomicCounter, DISABLED_test_multiple_thread) {
  // TODO (Herrera)
}

}  // namespace
