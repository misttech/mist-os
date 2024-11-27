// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/allocator.h>
#include <lib/mistos/zx/event.h>
#include <lib/mistos/zx/object.h>
#include <lib/unittest/unittest.h>

#include <set>

namespace unit_testing {
namespace {

bool usable_in_containers() {
  BEGIN_TEST;

  std::set<zx::unowned_event, std::less<>, util::Allocator<zx::unowned_event>> set;
  zx::event event;
  ASSERT_OK(zx::event::create(0u, &event));
  set.emplace(event);
  EXPECT_EQ(set.size(), 1u);
  EXPECT_TRUE(*set.begin() == event.get());

  END_TEST;
}

bool borrow_returns_unowned_object_of_same_handle() {
  BEGIN_TEST;

  zx::event event;
  ASSERT_OK(zx::event::create(0u, &event));
  ASSERT_TRUE(event.get() == event.borrow());
  ASSERT_TRUE(zx::unowned_event(event) == event.borrow());

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(mistos_zx_object_test)
UNITTEST("usable in containers", unit_testing::usable_in_containers)
UNITTEST("borrow returns unowned object of same handle",
         unit_testing::borrow_returns_unowned_object_of_same_handle)
UNITTEST_END_TESTCASE(mistos_zx_object_test, "mistos_zx_object_test", "mistos zx object test")
