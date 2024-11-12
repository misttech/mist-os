// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_TRY_FROM_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_TRY_FROM_H_

#include <optional>

namespace mtl {

template <typename From, typename To>
std::optional<To> TryFrom(From value) {
  return std::nullopt;
}

template <>
std::optional<size_t> TryFrom(uint32_t value);

template <>
std::optional<size_t> TryFrom(int64_t value);

}  // namespace mtl

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_TRY_FROM_H_
