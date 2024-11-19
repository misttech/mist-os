// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/expando/expando.h>
#include <lib/mistos/util/default_construct.h>
#include <lib/unittest/unittest.h>

#include <fbl/ref_counted.h>

namespace unit_testing {
namespace {

struct MyStruct : public fbl::RefCounted<MyStruct> {
  starnix_sync::Mutex<int> counter{0};
};

bool basic_test() {
  BEGIN_TEST;

  expando::Expando expando;
  auto first = expando.Get<MyStruct>();
  {
    ASSERT_EQ(0, *first->counter.Lock());
    (*first->counter.Lock())++;
  }

  auto second = expando.Get<MyStruct>();
  {
    ASSERT_EQ(1, *first->counter.Lock());
  }

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(expando)
UNITTEST("basic test", unit_testing::basic_test)
UNITTEST_END_TESTCASE(expando, "expando", "Tests for expando")
