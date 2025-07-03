// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_UI_SCENIC_LIB_TYPES_UTIL_TO_UNDERLYING_H_
#define SRC_UI_SCENIC_LIB_TYPES_UTIL_TO_UNDERLYING_H_

#include <type_traits>

namespace types {

// Returns the underlying type.  Typically used for enum classes.
// TODO(https://fxbug.dev/425757020): when C++23 is available, replace with std::to_underlying.
template <typename E>
constexpr auto to_underlying(E e) noexcept {
  return static_cast<std::underlying_type_t<E>>(e);
}

}  // namespace types

#endif  // SRC_UI_SCENIC_LIB_TYPES_UTIL_TO_UNDERLYING_H_
