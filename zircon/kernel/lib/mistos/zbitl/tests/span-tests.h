// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZBITL_TESTS_SPAN_TESTS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZBITL_TESTS_SPAN_TESTS_H_

#include <lib/mistos/zx/vmo.h>
#include <lib/stdcompat/cstddef.h>
#include <lib/stdcompat/span.h>
#include <zircon/assert.h>

#include <memory>
#include <string_view>

#include <fbl/alloc_checker.h>
#include <fbl/string.h>
#include <ktl/byte.h>
#include <ktl/span.h>
#include <ktl/unique_ptr.h>
#include <zxtest/zxtest.h>

#include "tests.h"

template <typename T>
struct BasicStringViewTestTraits {
  using storage_type = std::basic_string_view<T>;
  using payload_type = std::basic_string_view<T>;

  static constexpr bool kDefaultConstructedViewHasStorageError = false;
  static constexpr bool kExpectExtensibility = false;
  static constexpr bool kExpectOneShotReads = true;
  static constexpr bool kExpectUnbufferedReads = true;
  static constexpr bool kExpectUnbufferedWrites = false;

  struct Context {
    storage_type TakeStorage() const {
      return {reinterpret_cast<const T*>(buff_.get()), size_ / sizeof(T)};
    }

    ktl::unique_ptr<ktl::byte[]> buff_;
    size_t size_ = 0;
  };

  static void Create(ktl::span<const char> data, size_t size, Context* context) {
    const size_t n = (size + sizeof(T) - 1) / sizeof(T);
    fbl::AllocChecker ac;
    ktl::unique_ptr<ktl::byte[]> buff{new (ac) ktl::byte[n]};
    ZX_ASSERT(ac.check());
    memcpy(buff.get(), data.data(), n);
    *context = {std::move(buff), n};
  }

  static void Create(zx::vmo vmo, size_t size, Context* context) {
    ASSERT_TRUE(vmo.is_valid());
    const size_t n = (size + sizeof(T) - 1) / sizeof(T);
    fbl::AllocChecker ac;
    ktl::unique_ptr<ktl::byte[]> buff{new (ac) std::byte[n]};
    ZX_ASSERT(ac.check());
    ASSERT_EQ(vmo.read(buff.get(), 0, size), ZX_OK);
    *context = {std::move(buff), n};
  }

  static void Read(storage_type storage, payload_type payload, size_t size, fbl::String* contents) {
    *contents =
        fbl::String{reinterpret_cast<const char*>(payload.data()), payload.size() * sizeof(T)};
  }
};

using StringTestTraits = BasicStringViewTestTraits<char>;

template <typename T>
struct SpanTestTraits {
  using storage_type = cpp20::span<T>;
  using payload_type = storage_type;

  static constexpr bool kDefaultConstructedViewHasStorageError = false;
  static constexpr bool kExpectExtensibility = false;
  static constexpr bool kExpectOneShotReads = true;
  static constexpr bool kExpectUnbufferedReads = true;
  static constexpr bool kExpectUnbufferedWrites = !std::is_const_v<T>;

  struct Context {
    storage_type TakeStorage() const {
      return {reinterpret_cast<T*>(buff_.get()), size_ / sizeof(T)};
    }

    ktl::unique_ptr<std::byte[]> buff_;
    size_t size_ = 0;
  };

  static void Create(size_t size, Context* context) {
    const size_t n = (size + sizeof(T) - 1) / sizeof(T);
    fbl::AllocChecker ac;
    ktl::unique_ptr<std::byte[]> buff{new (ac) std::byte[n]};
    ZX_ASSERT(ac.check());
    *context = {std::move(buff), n};
  }

  static void Create(ktl::span<const char> data, size_t size, Context* context) {
    ASSERT_LE(data.size(), size);
    ASSERT_NO_FATAL_FAILURE(Create(size, context));
    memcpy(context->buff_.get(), data.data(), context->size_);
  }

  static void Create(zx::vmo vmo, size_t size, Context* context) {
    ASSERT_NO_FATAL_FAILURE(Create(size, context));
    ASSERT_EQ(vmo.read(context->buff_.get(), 0, context->size_), ZX_OK);
  }

  static void Read(const storage_type& storage, payload_type payload, size_t size,
                   fbl::String* contents) {
    auto bytes = cpp20::as_bytes(payload);
    ASSERT_LE(size, bytes.size());
    *contents = fbl::String{reinterpret_cast<const char*>(payload.data()), payload.size()};
  }

  static void Write(storage_type& storage, uint32_t offset, const fbl::String& data) {
    ASSERT_LT(offset, storage.size());
    ASSERT_LE(offset, storage.size() - data.size());
    memcpy(storage.data() + offset, data.data(), data.size());
  }

  static void ToPayload(const storage_type& storage, uint32_t offset, payload_type& payload) {
    ASSERT_LT(offset, storage.size());
    payload = storage.subspan(offset);
  }
};

using ByteSpanTestTraits = SpanTestTraits<std::byte>;

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZBITL_TESTS_SPAN_TESTS_H_
