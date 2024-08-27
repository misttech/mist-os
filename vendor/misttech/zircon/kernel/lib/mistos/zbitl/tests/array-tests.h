// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZBITL_TESTS_ARRAY_TESTS_H
#define ZIRCON_KERNEL_LIB_MISTOS_ZBITL_TESTS_ARRAY_TESTS_H

#include <lib/zbitl/memory.h>
#include <string.h>

#include <fbl/alloc_checker.h>
#include <fbl/string.h>

#include "span-tests.h"
#include "tests.h"

template <typename T>
struct FblArrayTestTraits {
  using storage_type = fbl::Array<T>;
  using payload_type = cpp20::span<T>;
  using creation_traits = FblArrayTestTraits;
  using SpanTraits = SpanTestTraits<T>;

  static constexpr bool kDefaultConstructedViewHasStorageError = false;
  static constexpr bool kExpectExtensibility = true;
  static constexpr bool kExpectOneShotReads = true;
  static constexpr bool kExpectUnbufferedReads = true;
  static constexpr bool kExpectUnbufferedWrites = !std::is_const_v<T>;

  struct Context {
    storage_type TakeStorage() { return std::move(storage_); }

    storage_type storage_;
  };

  static void Create(size_t size, Context* context) {
    const size_t n = (size + sizeof(T) - 1) / sizeof(T);
    fbl::AllocChecker ac;
    storage_type storage{new (ac) T[n], n};
    ZX_ASSERT(ac.check());
    *context = {std::move(storage)};
  }

  static void Create(ktl::span<const char> data, size_t size, Context* context) {
    ASSERT_LE(data.size(), size);
    ASSERT_NO_FATAL_FAILURE(Create(data.size(), context));
    memcpy(context->storage_.data(), data.data(), data.size());
  }

  static void Create(zx::vmo vmo, size_t size, Context* context) {
    ASSERT_NO_FATAL_FAILURE(Create(size, context));
    ASSERT_EQ(vmo.read(context->storage_.data(), 0, context->storage_.size()), ZX_OK);
  }

  static void Read(const storage_type& storage, payload_type payload, size_t size,
                   fbl::String* contents) {
    auto span = zbitl::AsSpan<T>(storage);
    ASSERT_NO_FATAL_FAILURE(SpanTraits::Read(span, payload, size, contents));
  }

  static void Write(storage_type& storage, uint32_t offset, const fbl::String& data) {
    auto span = zbitl::AsSpan<T>(storage);
    ASSERT_NO_FATAL_FAILURE(SpanTraits::Write(span, offset, data));
  }

  static void ToPayload(const storage_type& storage, uint32_t offset, payload_type& payload) {
    ASSERT_LT(offset, storage.size());
    payload = payload_type{storage.data() + offset, storage.size() - offset};
  }
};

using FblByteArrayTestTraits = FblArrayTestTraits<std::byte>;

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZBITL_TESTS_ARRAY_TESTS_H
