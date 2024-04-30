// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/mistos/util/strings/split_string.h>

#include <string_view>

#include <zxtest/zxtest.h>

namespace util {
namespace {

TEST(StringUtil, SplitString) {
  std::string_view sw = ",First,\tSecond,Third\t ,, Fou rth,\t";
  std::vector<fbl::String> r1 = {"", "First", "\tSecond", "Third\t ", "", " Fou rth", "\t"};
  std::vector<fbl::String> r2 = {"", "First", "Second", "Third", "", "Fou rth", ""};
  std::vector<fbl::String> r3 = {"First", "Second", "Third", "Fou rth"};
  std::vector<fbl::String> r4 = {"First", "\tSecond", "Third\t ", " Fou rth", "\t"};

  EXPECT_EQ(r1, SplitStringCopy(sw, ",", kKeepWhitespace, kSplitWantAll));
  EXPECT_EQ(r2, SplitStringCopy(sw, ",", kTrimWhitespace, kSplitWantAll));
  EXPECT_EQ(r3, SplitStringCopy(sw, ",", kTrimWhitespace, kSplitWantNonEmpty));
  EXPECT_EQ(r4, SplitStringCopy(sw, ",", kKeepWhitespace, kSplitWantNonEmpty));
}

}  // namespace
}  // namespace util
