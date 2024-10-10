// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_MAYBE_UNINIT_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_MAYBE_UNINIT_H_

// https://chatgpt.com/share/6707c679-b748-8001-8751-c9f02f9de762

#include <cstdint>
#include <optional>
#include <type_traits>

namespace mtl {

template <typename T>
class MaybeUninit {
 public:
  // Static method to create uninitialized MaybeUninit<T>
  static constexpr MaybeUninit<T> uninit() { return MaybeUninit<T>(); }

  T assume_init() && {
    // Move the value out, assuming it is initialized
    return std::move(*get_ptr());
  }

  // Returns a mutable pointer to the underlying uninitialized memory
  T* as_mut_ptr() { return get_ptr(); }

 private:
  // Constexpr constructor to create uninitialized memory
  constexpr MaybeUninit() : storage_() {}

  // Returns a pointer to the memory
  T* get_ptr() { return reinterpret_cast<T*>(&storage_); }

  // Storage to hold an uninitialized object of type T
  std::aligned_storage_t<sizeof(T), alignof(T)> storage_;
};

}  // namespace mtl

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_MAYBE_UNINIT_H_
