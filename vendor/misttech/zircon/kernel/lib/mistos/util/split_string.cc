// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/util/strings/split_string.h"

#include <lib/mistos/util/bstring.h>
#include <lib/mistos/util/strings/trim.h>
#include <zircon/assert.h>

#include <fbl/alloc_checker.h>
#include <fbl/string.h>

#include <ktl/enforce.h>

namespace util {
namespace {

template <typename OutputType>
OutputType PieceToOutputType(ktl::string_view view) {
  return view;
}

template <>
BString PieceToOutputType<BString>(ktl::string_view view) {
  return BString(view.data(), view.size());
}

size_t FindFirstOf(ktl::string_view view, char c, size_t pos) { return view.find(c, pos); }

size_t FindFirstOf(ktl::string_view view, ktl::string_view one_of, size_t pos) {
  return view.find_first_of(one_of, pos);
}

template <typename Str, typename OutputStringType, typename DelimiterType>
fbl::Vector<OutputStringType> SplitStringT(Str src, DelimiterType delimiter,
                                           WhiteSpaceHandling whitespace, SplitResult result_type) {
  fbl::Vector<OutputStringType> result;
  if (src.empty())
    return result;

  size_t start = 0;
  while (start != Str::npos) {
    size_t end = FindFirstOf(src, delimiter, start);

    ktl::string_view view;
    if (end == Str::npos) {
      view = src.substr(start);
      start = Str::npos;
    } else {
      view = src.substr(start, end - start);
      start = end + 1;
    }
    if (whitespace == kTrimWhitespace) {
      view = TrimString(view, " \t\r\n");
    }
    if (result_type == kSplitWantAll || !view.empty()) {
      fbl::AllocChecker ac;
      result.push_back(PieceToOutputType<OutputStringType>(view), &ac);
      ZX_ASSERT(ac.check());
    }
  }
  return result;
}

}  // namespace

fbl::Vector<BString> SplitStringCopy(ktl::string_view input, ktl::string_view separators,
                                     WhiteSpaceHandling whitespace, SplitResult result_type) {
  if (separators.size() == 1) {
    return SplitStringT<ktl::string_view, BString, char>(input, separators[0], whitespace,
                                                         result_type);
  }
  return SplitStringT<ktl::string_view, BString, ktl::string_view>(input, separators, whitespace,
                                                                   result_type);
}
fbl::Vector<ktl::string_view> SplitString(ktl::string_view input, ktl::string_view separators,
                                          WhiteSpaceHandling whitespace, SplitResult result_type) {
  if (separators.size() == 1) {
    return SplitStringT<ktl::string_view, ktl::string_view, char>(input, separators[0], whitespace,
                                                                  result_type);
  }
  return SplitStringT<ktl::string_view, ktl::string_view, ktl::string_view>(
      input, separators, whitespace, result_type);
}

}  // namespace util
