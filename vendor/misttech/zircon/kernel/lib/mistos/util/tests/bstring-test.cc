// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/bstring.h>
#include <lib/mistos/util/testing/unittest.h>
#include <lib/unittest/unittest.h>

namespace unit_testing {
namespace {

using mtl::BString;

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
    BString empty("abcde", static_cast<size_t>(0u));

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

bool format() {
  BEGIN_TEST;

  {
    // Basic formatting
    BString str = mtl::format("Hello %s", "world");
    EXPECT_STREQ("Hello world", str.data());
    EXPECT_EQ(11u, str.length());
  }

  {
    // Multiple arguments
    BString str = mtl::format("%d %s %d", 42, "test", 123);
    EXPECT_STREQ("42 test 123", str.data());
    EXPECT_EQ(11u, str.length());
  }

  {
    // Empty format string
    BString str = mtl::format("");
    EXPECT_STREQ("", str.data());
    EXPECT_EQ(0u, str.length());
  }

  {
    // Format error case - buffer overflow
    char long_str[1024];
    memset(long_str, 'a', sizeof(long_str) - 1);
    long_str[sizeof(long_str) - 1] = '\0';
    BString str = mtl::format("%s", long_str);
    EXPECT_STREQ("format error", str.data());
  }

  END_TEST;
}

bool vector_to_string() {
  BEGIN_TEST;

  {
    // Empty vector
    fbl::Vector<BString> vec;
    BString str = mtl::to_string(vec);
    EXPECT_STREQ("[]", str.data());
    EXPECT_EQ(2u, str.length());
  }

  {
    // Single element vector
    fbl::Vector<BString> vec;
    fbl::AllocChecker ac;
    vec.push_back(BString("test"), &ac);
    ASSERT_TRUE(ac.check());

    BString str = mtl::to_string(vec);
    EXPECT_STREQ("[test]", str.data());
    EXPECT_EQ(6u, str.length());
  }

  {
    // Multiple element vector
    fbl::Vector<BString> vec;
    fbl::AllocChecker ac;
    vec.push_back(BString("abc"), &ac);
    ASSERT_TRUE(ac.check());
    vec.push_back(BString("123"), &ac);
    ASSERT_TRUE(ac.check());
    vec.push_back(BString("xyz"), &ac);
    ASSERT_TRUE(ac.check());

    BString str = mtl::to_string(vec);
    EXPECT_STREQ("[abc, 123, xyz]", str.data());
    EXPECT_EQ(15u, str.length());
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
UNITTEST("format", unit_testing::format)
UNITTEST("vector to string", unit_testing::vector_to_string)
UNITTEST_END_TESTCASE(mistos_util_bstring, "mistos_util_bstring", "Tests BString")
