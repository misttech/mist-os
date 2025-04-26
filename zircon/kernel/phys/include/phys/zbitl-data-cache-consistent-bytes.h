// Copyright 2025 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ZBITL_CACHE_CONSISTENT_BYTES_H_
#define ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ZBITL_CACHE_CONSISTENT_BYTES_H_

#include <lib/arch/cache.h>
#include <lib/fit/result.h>
#include <lib/zbitl/storage-traits.h>
#include <stdint.h>

#include <ktl/byte.h>
#include <ktl/span.h>
#include <ktl/utility.h>

class DataCacheConsistentBytes {
 public:
  DataCacheConsistentBytes() = default;

  explicit DataCacheConsistentBytes(ktl::span<const ktl::byte> bytes) : bytes_(bytes) {}

  constexpr ktl::span<const ktl::byte> get() {
    if (!bytes_.empty()) [[likely]] {
      arch::SyncDataRange(reinterpret_cast<uintptr_t>(bytes_.data()), bytes_.size_bytes());
    }
    return bytes_;
  }

  constexpr ktl::span<const ktl::byte> take() && { return ktl::exchange(bytes_, {}); }

 private:
 // Allows the traits to access the raw bytes without sync'ing the entire
 // range, which should only occur on reads or direct payload usage.
  friend struct zbitl::StorageTraits<DataCacheConsistentBytes>;

  ktl::span<const ktl::byte> bytes_;
};

template <>
struct zbitl::StorageTraits<DataCacheConsistentBytes> {
  using Storage = DataCacheConsistentBytes;
  using ByteTraits = zbitl::StorageTraits<ktl::span<const ktl::byte>>;

  using error_type = ByteTraits::error_type;

  using payload_type = Storage;

  static std::string_view error_string(error_type error) { return {}; }

  static fit::result<error_type, uint32_t> Capacity(Storage& zbi) {
    return ByteTraits::Capacity(zbi.bytes_);
  }

  static fit::result<error_type, payload_type> Payload(Storage& zbi, uint32_t offset,
                                                       uint32_t length) {
    fit::result result = ByteTraits::Payload(zbi.bytes_, offset, length);
    ZX_DEBUG_ASSERT(result.is_ok());
    return fit::ok(DataCacheConsistentBytes{result.value()});
  }

  template <typename U, bool LowLocality>
  static std::enable_if_t<(alignof(U) <= kStorageAlignment),
                          fit::result<error_type, cpp20::span<const U>>>
  Read(Storage& zbi, payload_type payload, uint32_t length) {
    ktl::span consistent_payload = payload.get();
    return ByteTraits::Read<U, LowLocality>(zbi.bytes_, consistent_payload, length);
  }
};

#endif  // ZIRCON_KERNEL_PHYS_INCLUDE_PHYS_ZBITL_CACHE_CONSISTENT_BYTES_H_
