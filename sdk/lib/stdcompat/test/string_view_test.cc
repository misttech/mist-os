// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/stdcompat/string_view.h>

#include <cstring>
#include <limits>
#include <sstream>
#include <stdexcept>
#include <string>

#include "gtest.h"
#include "test_helper.h"

namespace {

TEST(StringViewTest, StartsWith) {
  constexpr cpp17::string_view kString = "ABCdef";

  // By convention, a string view always "starts" with an empty or NUL string.
  EXPECT_TRUE(cpp20::starts_with(kString, cpp17::string_view{}));
  EXPECT_TRUE(cpp20::starts_with(kString, ""));
  EXPECT_TRUE(cpp20::starts_with(kString, cpp17::string_view{""}));
  EXPECT_TRUE(cpp20::starts_with(kString, std::string{""}));

  EXPECT_TRUE(cpp20::starts_with(kString, 'A'));
  EXPECT_FALSE(cpp20::starts_with(kString, 'B'));
  EXPECT_FALSE(cpp20::starts_with(kString, 'f'));

  EXPECT_TRUE(cpp20::starts_with(kString, "A"));
  EXPECT_TRUE(cpp20::starts_with(kString, cpp17::string_view{"A"}));
  EXPECT_TRUE(cpp20::starts_with(kString, std::string{"A"}));

  EXPECT_TRUE(cpp20::starts_with(kString, "AB"));
  EXPECT_TRUE(cpp20::starts_with(kString, cpp17::string_view{"AB"}));
  EXPECT_TRUE(cpp20::starts_with(kString, "ABC"));
  EXPECT_TRUE(cpp20::starts_with(kString, cpp17::string_view{"ABC"}));
  EXPECT_TRUE(cpp20::starts_with(kString, "ABCd"));
  EXPECT_TRUE(cpp20::starts_with(kString, cpp17::string_view{"ABCd"}));
  EXPECT_TRUE(cpp20::starts_with(kString, "ABCde"));
  EXPECT_TRUE(cpp20::starts_with(kString, cpp17::string_view{"ABCde"}));

  // A string view should start with itself.
  EXPECT_TRUE(cpp20::starts_with(kString, "ABCdef"));
  EXPECT_TRUE(cpp20::starts_with(kString, kString));
  EXPECT_TRUE(cpp20::starts_with(kString, "ABCdef\0"));
  EXPECT_TRUE(cpp20::starts_with(kString, cpp17::string_view{"ABCdef\0"}));

  EXPECT_FALSE(cpp20::starts_with(kString, "rAnDoM"));
  EXPECT_FALSE(cpp20::starts_with(kString, cpp17::string_view{"rAnDoM"}));
  EXPECT_FALSE(cpp20::starts_with(kString, "longer than kString"));
  EXPECT_FALSE(cpp20::starts_with(kString, cpp17::string_view{"longer than kString"}));
}

TEST(StringViewTest, EndsWith) {
  constexpr cpp17::string_view kString = "ABCdef";

  // By convention, a string view always "ends" with an empty or NUL string.
  EXPECT_TRUE(cpp20::ends_with(kString, cpp17::string_view{}));
  EXPECT_TRUE(cpp20::ends_with(kString, ""));
  EXPECT_TRUE(cpp20::ends_with(kString, cpp17::string_view{""}));
  EXPECT_TRUE(cpp20::ends_with(kString, std::string{""}));

  EXPECT_TRUE(cpp20::ends_with(kString, 'f'));
  EXPECT_FALSE(cpp20::ends_with(kString, 'e'));
  EXPECT_FALSE(cpp20::ends_with(kString, 'A'));

  EXPECT_TRUE(cpp20::ends_with(kString, "f"));
  EXPECT_TRUE(cpp20::ends_with(kString, cpp17::string_view{"f"}));
  EXPECT_TRUE(cpp20::ends_with(kString, std::string{"f"}));
  EXPECT_TRUE(cpp20::ends_with(kString, "ef"));
  EXPECT_TRUE(cpp20::ends_with(kString, cpp17::string_view{"ef"}));
  EXPECT_TRUE(cpp20::ends_with(kString, "def"));
  EXPECT_TRUE(cpp20::ends_with(kString, cpp17::string_view{"def"}));
  EXPECT_TRUE(cpp20::ends_with(kString, "Cdef"));
  EXPECT_TRUE(cpp20::ends_with(kString, cpp17::string_view{"Cdef"}));
  EXPECT_TRUE(cpp20::ends_with(kString, "BCdef"));
  EXPECT_TRUE(cpp20::ends_with(kString, cpp17::string_view{"BCdef"}));

  // A string view should end with itself.
  EXPECT_TRUE(cpp20::ends_with(kString, "ABCdef"));
  EXPECT_TRUE(cpp20::ends_with(kString, kString));
  EXPECT_TRUE(cpp20::ends_with(kString, "ABCdef\0"));
  EXPECT_TRUE(cpp20::ends_with(kString, cpp17::string_view{"ABCdef\0"}));

  EXPECT_FALSE(cpp20::ends_with(kString, "rAnDoM"));
  EXPECT_FALSE(cpp20::ends_with(kString, cpp17::string_view{"rAnDoM"}));
  EXPECT_FALSE(cpp20::ends_with(kString, "longer than kString"));
  EXPECT_FALSE(cpp20::ends_with(kString, cpp17::string_view{"longer than kString"}));
}

}  // namespace
