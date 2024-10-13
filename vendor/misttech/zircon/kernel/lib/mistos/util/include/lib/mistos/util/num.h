// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_NUM_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_NUM_H_

#include <optional>

namespace mtl {

template <typename T>
auto overflowing_add(const T& a, const T& b) -> std::pair<T, bool> {
  T result;
  bool overflow = add_overflow(a, b, &result);
  return std::make_pair(result, overflow);
}

template <typename T>
auto overflowing_sub(const T& a, const T& b) -> std::pair<T, bool> {
  T result;
  bool overflow = sub_overflow(a, b, &result);
  return std::make_pair(result, overflow);
}

template <typename T>
auto checked_add(const T& a, const T& b) -> std::optional<T> {
  auto [result, overflow] = overflowing_add(a, b);
  if (unlikely(overflow)) {
    return std::nullopt;
  }
  return result;
}

template <typename T>
auto checked_sub(const T& a, const T& b) -> std::optional<T> {
  auto [result, overflow] = overflowing_sub(a, b);
  if (unlikely(overflow)) {
    return std::nullopt;
  }
  return result;
}

}  // namespace mtl

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_NUM_H_
