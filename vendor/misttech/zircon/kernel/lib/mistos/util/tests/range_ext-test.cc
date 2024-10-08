// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/range_ext.h>
#include <lib/unittest/unittest.h>

#include <ktl/array.h>
#include <ktl/span.h>

namespace unit_testing {

namespace {

bool no_intersection() {
  BEGIN_TEST;
  util::Range<uint32_t> a{.start = 1, .end = 3};
  util::Range<uint32_t> b{.start = 3, .end = 5};
  ASSERT_TRUE(intersect(a, b).is_empty());
  ASSERT_TRUE(intersect(b, a).is_empty());
  END_TEST;
}

bool partial_intersection() {
  BEGIN_TEST;
  util::Range<uint32_t> a{.start = 1, .end = 3};
  util::Range<uint32_t> b{.start = 2, .end = 5};
  ASSERT_TRUE(intersect(a, b) == util::Range<uint32_t>(2, 3));
  ASSERT_TRUE(intersect(b, a) == util::Range<uint32_t>(2, 3));
  END_TEST;
}

bool full_intersection() {
  BEGIN_TEST;
  util::Range<uint32_t> a{.start = 1, .end = 4};
  util::Range<uint32_t> b{.start = 2, .end = 3};
  ASSERT_TRUE(intersect(a, b) == util::Range<uint32_t>(2, 3));
  ASSERT_TRUE(intersect(b, a) == util::Range<uint32_t>(2, 3));

  util::Range<uint32_t> c{.start = 2, .end = 4};
  ASSERT_TRUE(intersect(a, c) == util::Range<uint32_t>(2, 4));
  ASSERT_TRUE(intersect(c, a) == util::Range<uint32_t>(2, 4));

  util::Range<uint32_t> d{.start = 1, .end = 2};
  ASSERT_TRUE(intersect(a, d) == util::Range<uint32_t>(1, 2));
  ASSERT_TRUE(intersect(d, a) == util::Range<uint32_t>(1, 2));

  ASSERT_TRUE(intersect(a, a) == util::Range<uint32_t>(1, 4));

  END_TEST;
}

}  // namespace

}  // namespace unit_testing

UNITTEST_START_TESTCASE(mistos_util_range_ext)
UNITTEST("no intersection", unit_testing::no_intersection)
UNITTEST("partial intersection", unit_testing::partial_intersection)
UNITTEST("full intersection", unit_testing::full_intersection)
UNITTEST_END_TESTCASE(mistos_util_range_ext, "mistos_util_range_ext", "Tests Range Ext")
