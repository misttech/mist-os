// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ALLOCATOR_H_
#define VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ALLOCATOR_H_

#include <lib/heap.h>
#include <zircon/assert.h>

#include <cstddef>

namespace util {

template <typename T>
struct Allocator {
  using value_type = T;

  Allocator() noexcept = default;

  template <class U>
  Allocator(const Allocator<U>&) noexcept {}

  T* allocate(std::size_t n) {
    if (n > std::size_t(-1) / sizeof(T)) {
      ZX_PANIC("allocate");
    }
    auto p = static_cast<T*>(malloc(n * sizeof(T)));
    ASSERT(p != nullptr);
    return p;
  }

  void deallocate(T* p, std::size_t) noexcept { free(p); }
};

}  // namespace util

#endif  // VENDOR_MISTTECH_ZIRCON_KERNEL_LIB_MISTOS_UTIL_INCLUDE_LIB_MISTOS_UTIL_ALLOCATOR_H_
