// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_STRINGS_SPLIT_STRING_H_
#define ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_STRINGS_SPLIT_STRING_H_

#include <lib/mistos/util/bstring.h>

#include <fbl/vector.h>
#include <ktl/string_view.h>

namespace util {

enum WhiteSpaceHandling : uint8_t {
  kKeepWhitespace,
  kTrimWhitespace,
};

enum SplitResult : uint8_t {
  // Strictly return all results.
  kSplitWantAll,

  // Only nonempty results will be added to the results.
  kSplitWantNonEmpty,
};

// Split the given string on ANY of the given separators, returning copies of
// the result
fbl::Vector<mtl::BString> SplitStringCopy(ktl::string_view input, ktl::string_view separators,
                                          WhiteSpaceHandling whitespace, SplitResult result_type);

// Like SplitStringCopy above except it returns a vector of std::string_views which
// reference the original buffer without copying.
fbl::Vector<std::string_view> SplitString(ktl::string_view input, ktl::string_view separators,
                                          WhiteSpaceHandling whitespace, SplitResult result_type);

}  // namespace util

#endif  // ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_STRINGS_SPLIT_STRING_H_
