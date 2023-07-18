// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_TRIVIAL_ALLOCATOR_INCLUDE_LIB_TRIVIAL_ALLOCATOR_INTERNAL_H_
#define SRC_LIB_TRIVIAL_ALLOCATOR_INCLUDE_LIB_TRIVIAL_ALLOCATOR_INTERNAL_H_

#include <cstddef>
#include <cstdint>

namespace trivial_allocator::internal {

// This is the same as std::align, but this library can't rely on anything from
// libc++ that's not always inlined.
inline void* Align(size_t alignment, size_t size, void*& ptr, size_t& space) {
  const uintptr_t start = reinterpret_cast<uintptr_t>(ptr);
  const uintptr_t aligned = (start + alignment - 1) & -alignment;
  const uintptr_t padding = aligned - start;
  if (space < size || space - size < padding) {
    return nullptr;
  }
  space -= padding;
  ptr = reinterpret_cast<void*>(aligned);
  return ptr;
}

}  // namespace trivial_allocator::internal

#endif  // SRC_LIB_TRIVIAL_ALLOCATOR_INCLUDE_LIB_TRIVIAL_ALLOCATOR_INTERNAL_H_
