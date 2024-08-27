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

  auto s1 = SplitStringCopy(sw, ",", kKeepWhitespace, kSplitWantAll);
  std::vector<fbl::String> result;
  std::transform(s1.begin(), s1.end(), std::inserter(result, result.end()),
                 [](const auto& str) { return str; });

  EXPECT_EQ(r1, result);
  result.clear();

  auto s2 = SplitStringCopy(sw, ",", kTrimWhitespace, kSplitWantAll);
  std::transform(s2.begin(), s2.end(), std::inserter(result, result.end()),
                 [](const auto& str) { return str; });
  EXPECT_EQ(r2, result);
  result.clear();

  auto s3 = SplitStringCopy(sw, ",", kTrimWhitespace, kSplitWantNonEmpty);
  std::transform(s3.begin(), s3.end(), std::inserter(result, result.end()),
                 [](const auto& str) { return str; });
  EXPECT_EQ(r3, result);
  result.clear();

  auto s4 = SplitStringCopy(sw, ",", kKeepWhitespace, kSplitWantNonEmpty);
  std::transform(s4.begin(), s4.end(), std::inserter(result, result.end()),
                 [](const auto& str) { return str; });
  EXPECT_EQ(r4, result);
}

}  // namespace
}  // namespace util
