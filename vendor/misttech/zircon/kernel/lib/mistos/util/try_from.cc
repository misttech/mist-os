// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "lib/mistos/util/try_from.h"

namespace mtl {

template <>
std::optional<size_t> TryFrom(uint32_t value) {
  return static_cast<size_t>(value);
}

template <>
std::optional<size_t> TryFrom(int64_t value) {
  // Negative value cannot be converted to size_t
  if (value < 0) {
    return std::nullopt;
  }
  // Value is too large to be represented as size_t
  if (static_cast<uint64_t>(value) > std::numeric_limits<size_t>::max()) {
    return std::nullopt;
  }

  return static_cast<size_t>(value);
}

}  // namespace mtl
