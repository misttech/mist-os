// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/developer/debug/shared/string_util.h"

#include "lib/stdcompat/string_view.h"

namespace debug {

bool StringStartsWith(std::string_view str, std::string_view begins_with) {
  return cpp20::starts_with(str, begins_with);
}

bool StringEndsWith(std::string_view str, std::string_view ends_with) {
  return cpp20::ends_with(str, ends_with);
}

}  // namespace debug
