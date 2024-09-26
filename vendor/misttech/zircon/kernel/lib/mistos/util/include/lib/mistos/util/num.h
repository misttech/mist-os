// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_NUM_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_NUM_H_

#include <optional>

namespace mtl {

template <typename T>
auto checked_add(const T& a, const T& b) -> std::optional<T> {
  T result;
  if (add_overflow(a, b, &result)) {
    return std::nullopt;
  }
  return result;
}

template <typename T>
auto checked_sub(const T& a, const T& b) -> std::optional<T> {
  T result;
  if (sub_overflow(a, b, &result)) {
    return std::nullopt;
  }
  return result;
}

}  // namespace mtl

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_NUM_H_
