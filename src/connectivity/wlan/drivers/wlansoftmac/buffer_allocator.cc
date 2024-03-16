// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "buffer_allocator.h"

#include <zircon/assert.h>

namespace wlan::drivers {

void LogAllocationFail(Buffer::Size size) {
  BufferDebugger<SmallBufferAllocator, LargeBufferAllocator, HugeBufferAllocator,
                 kBufferDebugEnabled>::Fail(size);
}

std::unique_ptr<Buffer> GetBuffer(size_t len) {
  std::unique_ptr<Buffer> buffer;

  if (len <= kSmallBufferSize) {
    buffer = SmallBufferAllocator::New();
    if (buffer != nullptr) {
      return buffer;
    }
    LogAllocationFail(Buffer::Size::kSmall);
  }

  if (len <= kLargeBufferSize) {
    buffer = LargeBufferAllocator::New();
    if (buffer != nullptr) {
      return buffer;
    }
    LogAllocationFail(Buffer::Size::kLarge);
  }

  if (len <= kHugeBufferSize) {
    buffer = HugeBufferAllocator::New();
    if (buffer != nullptr) {
      return buffer;
    }
    LogAllocationFail(Buffer::Size::kHuge);
  }
  return nullptr;
}

}  // namespace wlan::drivers

// Definition of static slab allocators.
// TODO(tkilbourn): tune how many slabs we are willing to grow up to. Reasonably
// large limits chosen for now.
DECLARE_STATIC_SLAB_ALLOCATOR_STORAGE(::wlan::drivers::HugeBufferTraits,
                                      ::wlan::drivers::kHugeSlabs, true);
DECLARE_STATIC_SLAB_ALLOCATOR_STORAGE(::wlan::drivers::LargeBufferTraits,
                                      ::wlan::drivers::kLargeSlabs, true);
DECLARE_STATIC_SLAB_ALLOCATOR_STORAGE(::wlan::drivers::SmallBufferTraits,
                                      ::wlan::drivers::kSmallSlabs, true);
