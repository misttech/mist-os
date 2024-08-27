// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2020 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_ZBITL_TESTS_VMO_TESTS_H_
#define ZIRCON_KERNEL_LIB_MISTOS_ZBITL_TESTS_VMO_TESTS_H_

#include <lib/mistos/zbitl/vmo.h>

#include <fbl/string.h>
#include <ktl/span.h>
#include <zxtest/zxtest.h>

#include "tests.h"

// All VMO-related test traits create extensible VMOs by default,
// parameterizing all of the creation APIs with a boolean `Resizable` template
// parameter that defaults to true. Each set of traits gives
// `kExpectExtensibility = true` to account for this default behaviour in the
// general, traits-abstracted testing; more dedicated testing with
// `Resizable = false` is given in vmo-tests.cc.

struct VmoTestTraits {
  using storage_type = zx::vmo;
  using payload_type = uint64_t;
  using creation_traits = VmoTestTraits;

  static constexpr bool kDefaultConstructedViewHasStorageError = true;
  static constexpr bool kExpectExtensibility = true;  // See note at the top.
  static constexpr bool kExpectOneShotReads = false;
  static constexpr bool kExpectUnbufferedReads = true;
  static constexpr bool kExpectUnbufferedWrites = false;

  struct Context {
    storage_type TakeStorage() { return std::move(storage_); }

    storage_type storage_;
  };

  template <bool Resizable = true>
  static void Create(size_t size, Context* context) {
    zx::vmo vmo;
    ASSERT_EQ(zx::vmo::create(size, Resizable ? ZX_VMO_RESIZABLE : 0u, &vmo), ZX_OK);
    *context = {std::move(vmo)};
  }

  template <bool Resizable = true>
  static void Create(ktl::span<const char> buff, size_t size, Context* context) {
    ASSERT_LE(buff.size(), size);
    ASSERT_NO_FATAL_FAILURE(Create<Resizable>(size, context));
    ASSERT_EQ(context->storage_.write(buff.data(), 0u, size), ZX_OK);
  }

  static void Create(zx::vmo vmo, size_t size, Context* context) {
    ASSERT_TRUE(vmo.is_valid());
    context->storage_ = std::move(vmo);
  }

  static void Create(zx::vmo vmo, Context* context) {
    ASSERT_TRUE(vmo.is_valid());
    context->storage_ = std::move(vmo);
  }

  static void Read(const storage_type& storage, payload_type payload, size_t size,
                   fbl::String* contents) {
    // Always malloc one byte more to avoid malloc(0).
    char* tmp = static_cast<char*>(malloc(size + 1));
    auto defer = fit::defer([&tmp] { free(tmp); });
    ASSERT_TRUE(tmp != nullptr);
    ASSERT_EQ(ZX_OK, storage.read(tmp, payload, size));
    *contents += fbl::String(tmp, size);
  }

  static void Write(const storage_type& storage, uint32_t offset, const fbl::String& data) {
    ASSERT_EQ(ZX_OK, storage.write(data.data(), offset, data.size()));
  }

  static void ToPayload(const storage_type& storage, uint32_t offset, payload_type& payload) {
    payload = static_cast<payload_type>(offset);
  }

  static const zx::vmo& GetVmo(const storage_type& storage) { return storage; }
};

struct UnownedVmoTestTraits {
  using storage_type = zx::unowned_vmo;
  using payload_type = uint64_t;
  using creation_traits = VmoTestTraits;

  static constexpr bool kDefaultConstructedViewHasStorageError = true;
  static constexpr bool kExpectExtensibility = true;  // See note at the top.
  static constexpr bool kExpectOneShotReads = false;
  static constexpr bool kExpectUnbufferedReads = true;
  static constexpr bool kExpectUnbufferedWrites = false;

  struct Context {
    storage_type TakeStorage() { return std::move(storage_); }

    storage_type storage_;
    zx::vmo keepalive_;
  };

  template <bool Resizable = true>
  static void Create(size_t size, Context* context) {
    typename VmoTestTraits::Context vmo_context;
    ASSERT_NO_FATAL_FAILURE(VmoTestTraits::Create<Resizable>(size, &vmo_context));
    context->storage_ = zx::unowned_vmo{vmo_context.storage_};
    context->keepalive_ = std::move(vmo_context.storage_);
  }

  template <bool Resizable = true>
  static void Create(ktl::span<const char> buff, size_t size, Context* context) {
    typename VmoTestTraits::Context vmo_context;
    ASSERT_NO_FATAL_FAILURE(VmoTestTraits::Create<Resizable>(std::move(buff), size, &vmo_context));
    context->storage_ = zx::unowned_vmo{vmo_context.storage_};
    context->keepalive_ = std::move(vmo_context.storage_);
  }

  static void Create(zx::vmo vmo, size_t size, Context* context) {
    Create(std::move(vmo), context);
  }

  static void Create(zx::vmo vmo, Context* context) {
    context->storage_ = zx::unowned_vmo{vmo};
    context->keepalive_ = std::move(vmo);
  }

  static void Read(const storage_type& storage, payload_type payload, size_t size,
                   fbl::String* contents) {
    ASSERT_NO_FATAL_FAILURE(VmoTestTraits::Read(*storage, payload, size, contents));
  }

  static void Write(const storage_type& storage, uint32_t offset, const fbl::String& data) {
    ASSERT_NO_FATAL_FAILURE(VmoTestTraits::Write(*storage, offset, data));
  }

  static void ToPayload(const storage_type& storage, uint32_t offset, payload_type& payload) {
    payload = static_cast<payload_type>(offset);
  }

  static const zx::vmo& GetVmo(const storage_type& storage) { return *storage; }
};

struct MapOwnedVmoTestTraits {
  using storage_type = zbitl::MapOwnedVmo;
  using payload_type = uint64_t;
  using creation_traits = MapOwnedVmoTestTraits;

  static constexpr bool kDefaultConstructedViewHasStorageError = true;
  static constexpr bool kExpectExtensibility = true;  // See note at the top.
  static constexpr bool kExpectOneShotReads = true;
  static constexpr bool kExpectUnbufferedReads = true;
  static constexpr bool kExpectUnbufferedWrites = true;

  struct Context {
    storage_type TakeStorage() { return std::move(storage_); }

    storage_type storage_;
  };

  template <bool Resizable = true>
  static void Create(size_t size, Context* context) {
    typename VmoTestTraits::Context vmo_context;
    ASSERT_NO_FATAL_FAILURE(VmoTestTraits::Create<Resizable>(size, &vmo_context));
    *context = {zbitl::MapOwnedVmo{std::move(vmo_context.storage_), /*writable=*/true}};
  }

  template <bool Resizable = true>
  static void Create(ktl::span<const char> buff, size_t size, Context* context) {
    typename VmoTestTraits::Context vmo_context;
    ASSERT_NO_FATAL_FAILURE(VmoTestTraits::Create<Resizable>(std::move(buff), size, &vmo_context));
    *context = {zbitl::MapOwnedVmo{vmo_context.TakeStorage(), /*writable=*/true}};
  }

  static void Create(zx::vmo vmo, Context* context) {
    typename VmoTestTraits::Context vmo_context;
    *context = {zbitl::MapOwnedVmo{std::move(vmo), /*writable=*/true}};
  }

  static void Create(zx::vmo vmo, size_t size, Context* context) {
    Create(std::move(vmo), context);
  }

  static void Read(const storage_type& storage, payload_type payload, size_t size,
                   fbl::String* contents) {
    ASSERT_NO_FATAL_FAILURE(VmoTestTraits::Read(storage.vmo(), payload, size, contents));
  }

  static void Write(const storage_type& storage, uint32_t offset, const fbl::String& data) {
    ASSERT_NO_FATAL_FAILURE(VmoTestTraits::Write(storage.vmo(), offset, data));
  }

  static void ToPayload(const storage_type& storage, uint32_t offset, payload_type& payload) {
    payload = static_cast<payload_type>(offset);
  }

  static const zx::vmo& GetVmo(const storage_type& storage) { return storage.vmo(); }
};

struct MapUnownedVmoTestTraits {
  using storage_type = zbitl::MapUnownedVmo;
  using payload_type = uint64_t;
  using creation_traits = MapOwnedVmoTestTraits;

  static constexpr bool kDefaultConstructedViewHasStorageError = true;
  static constexpr bool kExpectExtensibility = true;  // See note at the top.
  static constexpr bool kExpectOneShotReads = true;
  static constexpr bool kExpectUnbufferedReads = true;
  static constexpr bool kExpectUnbufferedWrites = true;

  struct Context {
    storage_type TakeStorage() { return std::move(storage_); }

    storage_type storage_;
    zx::vmo keepalive_;
  };

  template <bool Resizable = true>
  static void Create(ktl::span<const char> buff, size_t size, Context* context) {
    typename UnownedVmoTestTraits::Context unowned_vmo_context;
    ASSERT_NO_FATAL_FAILURE(
        UnownedVmoTestTraits::Create<Resizable>(std::move(buff), size, &unowned_vmo_context));
    context->storage_ = zbitl::MapUnownedVmo{std::move(unowned_vmo_context.storage_),
                                             /*writable=*/true};
    context->keepalive_ = std::move(unowned_vmo_context.keepalive_);
  }

  template <bool Resizable = true>
  static void Create(size_t size, Context* context) {
    typename UnownedVmoTestTraits::Context unowned_vmo_context;
    ASSERT_NO_FATAL_FAILURE(UnownedVmoTestTraits::Create<Resizable>(size, &unowned_vmo_context));
    *context = {zbitl::MapUnownedVmo{std::move(unowned_vmo_context.storage_),
                                     /*writable=*/true},
                std::move(unowned_vmo_context.keepalive_)};
  }

  static void Create(zx::vmo vmo, size_t size, Context* context) {
    typename UnownedVmoTestTraits::Context unowned_vmo_context;
    ASSERT_NO_FATAL_FAILURE(
        UnownedVmoTestTraits::Create(std::move(vmo), size, &unowned_vmo_context));
    context->storage_ = zbitl::MapUnownedVmo{std::move(unowned_vmo_context.storage_),
                                             /*writable=*/true};
    context->keepalive_ = std::move(unowned_vmo_context.keepalive_);
  }

  static void Create(zx::vmo vmo, Context* context) {
    typename UnownedVmoTestTraits::Context unowned_vmo_context;
    ASSERT_NO_FATAL_FAILURE(UnownedVmoTestTraits::Create(std::move(vmo), &unowned_vmo_context));
    context->storage_ = zbitl::MapUnownedVmo{std::move(unowned_vmo_context.storage_),
                                             /*writable=*/true};
    context->keepalive_ = std::move(unowned_vmo_context.keepalive_);
  }

  static void Read(const storage_type& storage, payload_type payload, size_t size,
                   fbl::String* contents) {
    ASSERT_NO_FATAL_FAILURE(VmoTestTraits::Read(storage.vmo(), payload, size, contents));
  }

  static void Write(const storage_type& storage, uint32_t offset, const fbl::String& data) {
    ASSERT_NO_FATAL_FAILURE(VmoTestTraits::Write(storage.vmo(), offset, data));
  }

  static void ToPayload(const storage_type& storage, uint32_t offset, payload_type& payload) {
    payload = static_cast<payload_type>(offset);
  }

  static const zx::vmo& GetVmo(const storage_type& storage) { return storage.vmo(); }
};

#endif  // ZIRCON_KERNEL_LIB_MISTOS_ZBITL_TESTS_VMO_TESTS_H_
