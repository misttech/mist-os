// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/e2e_tests/fuzzy_matcher.h"

#include <iostream>
#include <string>
#include <string_view>

#include "src/lib/fxl/strings/split_string.h"

namespace zxdb {

bool FuzzyMatcher::MatchesLine(const std::vector<std::string_view>& substrs) {
  while (content_) {
    std::string line;
    std::getline(content_, line);
    size_t pos = 0;
    for (auto& substr : substrs) {
      pos = line.find(substr, pos);
      if (pos == std::string::npos)
        break;
    }
    if (pos != std::string::npos)
      return true;
  }
  return false;
}

bool FuzzyMatcher::MatchesLine(std::string_view pattern) {
  return MatchesLine(
      fxl::SplitString(pattern, "??", fxl::kKeepWhitespace, fxl::kSplitWantNonEmpty));
}

}  // namespace zxdb
