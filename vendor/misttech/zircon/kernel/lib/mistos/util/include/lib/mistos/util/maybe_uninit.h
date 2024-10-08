// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_MAYBE_UNINIT_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_MAYBE_UNINIT_H_

#include <cstdint>
#include <optional>
#include <type_traits>

namespace mtl {

template <typename T>
class MaybeUninit {
 public:
  // Constexpr constructor to create uninitialized memory
  constexpr MaybeUninit() : storage_() {}

  // Static method to create uninitialized MaybeUninit<T>
  static constexpr MaybeUninit<T> uninit() { return MaybeUninit<T>(); }

  T assume_init() && {
    // Move the value out, assuming it is initialized
    return std::move(*get_ptr());
  }

 private:
  // Returns a pointer to the memory
  T* get_ptr() { return reinterpret_cast<T*>(&storage_); }

  std::aligned_storage_t<sizeof(T), alignof(T)> storage_;
};

}  // namespace mtl

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_MAYBE_UNINIT_H_
