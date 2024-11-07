// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ALLOCATOR_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ALLOCATOR_H_

#include <zircon/assert.h>

#include <cstddef>

#include <fbl/alloc_checker.h>

namespace util {

template <typename T>
struct Allocator {
  using value_type = T;

  Allocator() = default;

  // Converting constructor
  template <class U>
  Allocator(const Allocator<U>&) {}

  // Allocate memory
  T* allocate(std::size_t n) {
    if (n > static_cast<std::size_t>(-1) / sizeof(T)) {
      ZX_PANIC("allocate");
    }

    fbl::AllocChecker ac;
    void* raw = new (&ac) char[n * sizeof(T)];
    if (!ac.check()) {
      ZX_PANIC("failed to allocate memory");
    }
    return static_cast<T*>(raw);
  }

  // Deallocate memory
  void deallocate(T* p, std::size_t) noexcept { delete[] reinterpret_cast<char*>(p); }

  // Equality comparison for compatibility with std::allocator_traits
  template <typename U>
  bool operator==(const Allocator<U>&) const noexcept {
    return true;
  }

  template <typename U>
  bool operator!=(const Allocator<U>& other) const noexcept {
    return false;
  }
};

}  // namespace util

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ALLOCATOR_H_
