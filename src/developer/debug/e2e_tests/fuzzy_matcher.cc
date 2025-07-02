// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/e2e_tests/fuzzy_matcher.h"

#include <iostream>
#include <string>
#include <string_view>

#include "src/lib/fxl/strings/split_string.h"

namespace zxdb {

bool FuzzyMatcher::MatchesLine(const std::vector<std::string_view>& substrs,
                               bool allow_out_of_order) {
  while (content_) {
    std::string line;
    std::getline(content_, line);
    size_t pos = 0;
    for (auto& substr : substrs) {
      pos = line.find(substr, pos);
      if (pos == std::string::npos)
        break;
    }

    if (pos != std::string::npos) {
      if (allow_out_of_order && content_.peek()) {
        // Only reset the stream pointer to the beginning if out-of-order output is allowed and
        // there are still more bytes to read. We don't want to infinitely loop over the final
        // match.
        content_.seekg(0, std::ios_base::beg);
      }
      return true;
    }
  }
  return false;
}

bool FuzzyMatcher::MatchesLine(std::string_view pattern, bool allow_out_of_order) {
  return MatchesLine(fxl::SplitString(pattern, "??", fxl::kKeepWhitespace, fxl::kSplitWantNonEmpty),
                     allow_out_of_order);
}

}  // namespace zxdb
