// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/bstring.h>
#include <lib/mistos/util/testing/unittest.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {

namespace {

bool empty() {
  BEGIN_TEST;

  {
    BString empty;

    EXPECT_STREQ("", empty.data());
    EXPECT_STREQ("", empty.c_str());

    // EXPECT_EQ(0u, empty.length());
    EXPECT_EQ(0u, empty.size());
    EXPECT_TRUE(empty.empty());

    EXPECT_STREQ("", empty.begin());
    EXPECT_EQ(0u, empty.end() - empty.begin());
    EXPECT_STREQ("", empty.cbegin());
    EXPECT_EQ(0u, empty.cend() - empty.cbegin());

    EXPECT_EQ(0, empty[0u]);
  }

  {
    BString empty("");

    EXPECT_STREQ("", empty.data());
    EXPECT_STREQ("", empty.c_str());

    EXPECT_EQ(0u, empty.length());
    EXPECT_EQ(0u, empty.size());
    EXPECT_TRUE(empty.empty());

    EXPECT_STREQ("", empty.begin());
    EXPECT_EQ(0u, empty.end() - empty.begin());
    EXPECT_STREQ("", empty.cbegin());
    EXPECT_EQ(0u, empty.cend() - empty.cbegin());

    EXPECT_EQ(0, empty[0u]);
  }

  {
    BString empty("abcde", size_t(0u));

    EXPECT_STREQ("", empty.data());
    EXPECT_STREQ("", empty.c_str());

    EXPECT_EQ(0u, empty.length());
    EXPECT_EQ(0u, empty.size());
    EXPECT_TRUE(empty.empty());

    EXPECT_STREQ("", empty.begin());
    EXPECT_EQ(0u, empty.end() - empty.begin());
    EXPECT_STREQ("", empty.cbegin());
    EXPECT_EQ(0u, empty.cend() - empty.cbegin());

    EXPECT_EQ(0, empty[0u]);
  }

  {
    BString empty(ktl::string_view("abcde", 0u));

    EXPECT_STREQ("", empty.data());
    EXPECT_STREQ("", empty.c_str());

    EXPECT_EQ(0u, empty.length());
    EXPECT_EQ(0u, empty.size());
    EXPECT_TRUE(empty.empty());

    EXPECT_STREQ("", empty.begin());
    EXPECT_EQ(0u, empty.end() - empty.begin());
    EXPECT_STREQ("", empty.cbegin());
    EXPECT_EQ(0u, empty.cend() - empty.cbegin());

    EXPECT_EQ(0, empty[0u]);
  }

  END_TEST;
}

bool non_empty() {
  BEGIN_TEST;

  {
    BString str("abc");

    EXPECT_STREQ("abc", str.data());

    EXPECT_EQ(3u, str.length());
    EXPECT_EQ(3u, str.size());
    EXPECT_FALSE(str.empty());

    EXPECT_STREQ("abc", str.begin());
    EXPECT_EQ(3u, str.end() - str.begin());
    EXPECT_STREQ("abc", str.cbegin());
    EXPECT_EQ(3u, str.cend() - str.cbegin());

    EXPECT_EQ('b', str[1u]);
  }

  {
    BString str("abc", 2u);

    EXPECT_STREQ("ab", str.data());

    EXPECT_EQ(2u, str.length());
    EXPECT_EQ(2u, str.size());
    EXPECT_FALSE(str.empty());

    EXPECT_STREQ("ab", str.begin());
    EXPECT_EQ(2u, str.end() - str.begin());
    EXPECT_STREQ("ab", str.cbegin());
    EXPECT_EQ(2u, str.cend() - str.cbegin());

    EXPECT_EQ('b', str[1u]);
  }

  {
    BString str(std::string_view("abcdef", 2u));

    EXPECT_STREQ("ab", str.data());

    EXPECT_EQ(2u, str.length());
    EXPECT_EQ(2u, str.size());
    EXPECT_FALSE(str.empty());

    EXPECT_STREQ("ab", str.begin());
    EXPECT_EQ(2u, str.end() - str.begin());
    EXPECT_STREQ("ab", str.cbegin());
    EXPECT_EQ(2u, str.cend() - str.cbegin());

    EXPECT_EQ('b', str[1u]);
  }

  END_TEST;
}

bool copy_move_and_assignment() {
  BEGIN_TEST;

  {
    BString abc("abc");
    BString copy(abc);
    EXPECT_STREQ("abc", abc.data());
    EXPECT_STREQ("abc", copy.data());
    EXPECT_EQ(3u, copy.length());
  }

  {
    BString abc("abc");
    BString copy(abc);
    BString move(ktl::move(copy));
    EXPECT_STREQ("abc", abc.data());
    EXPECT_STREQ("", copy.data());
    EXPECT_STREQ("abc", move.data());
    EXPECT_EQ(3u, move.length());
  }

  {
    BString abc("abc");
    BString str;
    str = abc;
    EXPECT_STREQ("abc", abc.data());
    EXPECT_STREQ("abc", str.data());
    EXPECT_EQ(3u, str.length());
  }

  {
    BString abc("abc");
    BString copy(abc);
    BString str;
    str = std::move(copy);
    EXPECT_STREQ("abc", abc.data());
    EXPECT_STREQ("", copy.data());
    EXPECT_STREQ("abc", str.data());
    EXPECT_EQ(3u, str.length());
  }

  {
    BString str;
    str = "abc";
    EXPECT_STREQ("abc", str.data());
    EXPECT_EQ(3u, str.length());

    str = "";
    EXPECT_STREQ("", str.data());
    EXPECT_EQ(0u, str.length());

    BString copy(str);
    EXPECT_STREQ("", copy.data());
    EXPECT_EQ(0u, copy.length());

    BString move(copy);
    EXPECT_STREQ("", copy.data());
    EXPECT_EQ(0u, copy.length());
    EXPECT_STREQ("", move.data());
    EXPECT_EQ(0u, move.length());
  }

  END_TEST;
}

bool to_string() {
  BEGIN_TEST;

  {
    BString empty;
    std::string_view piece(empty);
    EXPECT_STREQ(empty.data(), piece.data());
    EXPECT_EQ(0u, piece.length());
  }

  {
    BString str("abc");
    std::string_view piece(str);
    EXPECT_STREQ(str.data(), piece.data());
    EXPECT_EQ(3u, piece.length());
  }

  END_TEST;
}

}  // namespace
}  // namespace unit_testing

UNITTEST_START_TESTCASE(mistos_util_bstring)
UNITTEST("test empty", unit_testing::empty)
UNITTEST("test non empty", unit_testing::non_empty)
UNITTEST("test copy move and assignment", unit_testing::copy_move_and_assignment)
UNITTEST("test to string", unit_testing::to_string)
UNITTEST_END_TESTCASE(mistos_util_bstring, "mistos_util_bstring", "Tests BString")
