// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/bstring.h>
#include <lib/mistos/util/strings/split_string.h>
#include <lib/unittest/unittest.h>

#include <string_view>

#include "fbl/alloc_checker.h"

namespace unit_testing {
namespace {

bool splitstring() {
  BEGIN_TEST;

  fbl::AllocChecker ac;
  ktl::string_view sw = ",First,\tSecond,Third\t ,, Fou rth,\t";

  fbl::Vector<BString> r1;
  r1.push_back("", &ac);
  ASSERT(ac.check());
  r1.push_back("First", &ac);
  ASSERT(ac.check());
  r1.push_back("\tSecond", &ac);
  ASSERT(ac.check());
  r1.push_back("Third\t ", &ac);
  ASSERT(ac.check());
  r1.push_back("", &ac);
  ASSERT(ac.check());
  r1.push_back(" Fou rth", &ac);
  ASSERT(ac.check());
  r1.push_back("\t", &ac);
  ASSERT(ac.check());

  fbl::Vector<BString> r2;
  r2.push_back("", &ac);
  ASSERT(ac.check());
  r2.push_back("First", &ac);
  ASSERT(ac.check());
  r2.push_back("Second", &ac);
  ASSERT(ac.check());
  r2.push_back("Third", &ac);
  ASSERT(ac.check());
  r2.push_back("", &ac);
  ASSERT(ac.check());
  r2.push_back("Fou rth", &ac);
  ASSERT(ac.check());
  r2.push_back("", &ac);
  ASSERT(ac.check());

  fbl::Vector<BString> r3;
  r3.push_back("First", &ac);
  ASSERT(ac.check());
  r3.push_back("Second", &ac);
  ASSERT(ac.check());
  r3.push_back("Third", &ac);
  ASSERT(ac.check());
  r3.push_back("Fou rth", &ac);
  ASSERT(ac.check());

  fbl::Vector<BString> r4;
  r4.push_back("First", &ac);
  ASSERT(ac.check());
  r4.push_back("\tSecond", &ac);
  ASSERT(ac.check());
  r4.push_back("Third\t ", &ac);
  ASSERT(ac.check());
  r4.push_back(" Fou rth", &ac);
  ASSERT(ac.check());
  r4.push_back("\t", &ac);
  ASSERT(ac.check());

  auto s1 = SplitStringCopy(sw, ",", util::kKeepWhitespace, util::kSplitWantAll);
  ASSERT_TRUE(s1.size() == r1.size());
  EXPECT_TRUE(s1[0] == r1[0]);
  EXPECT_TRUE(s1[1] == r1[1]);
  EXPECT_TRUE(s1[2] == r1[2]);
  EXPECT_TRUE(s1[3] == r1[3]);
  EXPECT_TRUE(s1[4] == r1[4]);
  EXPECT_TRUE(s1[5] == r1[5]);
  EXPECT_TRUE(s1[6] == r1[6]);

  auto s2 = SplitStringCopy(sw, ",", util::kTrimWhitespace, util::kSplitWantAll);
  ASSERT_TRUE(s2.size() == r2.size());
  EXPECT_TRUE(s2[0] == r2[0]);
  EXPECT_TRUE(s2[1] == r2[1]);
  EXPECT_TRUE(s2[2] == r2[2]);
  EXPECT_TRUE(s2[3] == r2[3]);
  EXPECT_TRUE(s2[4] == r2[4]);
  EXPECT_TRUE(s2[5] == r2[5]);
  EXPECT_TRUE(s2[6] == r2[6]);

  auto s3 = SplitStringCopy(sw, ",", util::kTrimWhitespace, util::kSplitWantNonEmpty);
  ASSERT_TRUE(s3.size() == r3.size());
  EXPECT_TRUE(s3[0] == r3[0]);
  EXPECT_TRUE(s3[1] == r3[1]);
  EXPECT_TRUE(s3[2] == r3[2]);
  EXPECT_TRUE(s3[3] == r3[3]);

  auto s4 = SplitStringCopy(sw, ",", util::kKeepWhitespace, util::kSplitWantNonEmpty);
  ASSERT_TRUE(s4.size() == r4.size());
  EXPECT_TRUE(s4[0] == r4[0]);
  EXPECT_TRUE(s4[1] == r4[1]);
  EXPECT_TRUE(s4[2] == r4[2]);
  EXPECT_TRUE(s4[3] == r4[3]);
  EXPECT_TRUE(s4[4] == r4[4]);

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(mistos_util_stringutil)
UNITTEST("test split string", unit_testing::splitstring)
UNITTEST_END_TESTCASE(mistos_util_stringutil, "mistos_util_stringutil", "Tests Util String")
