// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/util/strings/trim.h"

#include <ktl/enforce.h>

namespace util {

ktl::string_view TrimString(ktl::string_view str, ktl::string_view chars_to_trim) {
  size_t start_index = str.find_first_not_of(chars_to_trim);
  if (start_index == ktl::string_view::npos) {
    return ktl::string_view();
  }
  size_t end_index = str.find_last_not_of(chars_to_trim);
  return str.substr(start_index, end_index - start_index + 1);
}

}  // namespace util
