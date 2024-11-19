// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_TYPE_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_TYPE_H_

#include <string_view>

#define TYPE_HASH(type) mtl::hash(#type)

namespace mtl {

constexpr uint64_t hash(std::string_view str) { return std::hash<std::string_view>{}(str); }

template <typename T>
constexpr uint64_t typeid_hash() {
  return TYPE_HASH(T);
}

}  // namespace mtl

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_TYPE_H_
