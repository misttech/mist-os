// Copyright 2024 The Fuchsia Authors
//
// Use of this source code is governed by a MIT-style
// license that can be found in the LICENSE file or at
// https://opensource.org/licenses/MIT

#ifndef ZIRCON_KERNEL_VM_INCLUDE_VM_ATTRIBUTION_H_
#define ZIRCON_KERNEL_VM_INCLUDE_VM_ATTRIBUTION_H_

#include <cassert>

#include <ffl/fixed.h>

// Define this to enable legacy attribution codepaths (which requires usage of split bits instead of
// share counts when tracking COW pages).
//
// TODO(https://fxbug.dev/issues/338300808): Remove this flag when the new attribution code (and
// thus share counts) are the default.
#define ENABLE_LEGACY_ATTRIBUTION true

namespace vm {

// Structure to store fractional counts of bytes in fixed point with 63 bits of precision. The max
// fractional value is thus `2-Fraction::Epsilon()`. Counts are strictly unsigned.
//
// This structure supports accumulation with other fractional counts and the code will handle the
// internal bookkeeping around moving whole bytes from the fractional sum to the integral sum.
//
// The structure also supports addition, subtraction, and division with whole integers. If overflow
// occurs in either direction, an assert is fired. Multiplication is not supported.
//
// We always check and guarantee that any fractional sums >=1 have the excess byte stripped off
// and rolled over to the integral value. Thus, since accumulation always operates on fractions
// `<=1-Fraction::Epsilon()`, it always generates results `<= 2-Fraction::Epsilon()` and there is
// never overflow in the fractional fields.
struct FractionalBytes {
  using Fraction = ffl::Fixed<uint64_t, 63>;
  constexpr static Fraction kOneByte = Fraction(1);
  FractionalBytes() = default;

  constexpr explicit FractionalBytes(uint64_t whole_bytes) : integral(whole_bytes) {}
#if ENABLE_LEGACY_ATTRIBUTION
  constexpr explicit FractionalBytes(uint64_t numerator, uint64_t denominator)
      : integral(numerator / denominator) {}
#else
  constexpr explicit FractionalBytes(uint64_t numerator, uint64_t denominator)
      : integral(numerator / denominator),
        fractional((kOneByte / denominator) * (numerator % denominator)) {}
#endif

  FractionalBytes operator+(const uint64_t& other) const {
    FractionalBytes ret{*this};
    ret += other;
    return ret;
  }
  FractionalBytes operator-(const uint64_t& other) const {
    FractionalBytes ret{*this};
    ret -= other;
    return ret;
  }
  FractionalBytes operator/(const uint64_t& other) const {
    FractionalBytes ret{*this};
    ret /= other;
    return ret;
  }
  FractionalBytes& operator+=(const uint64_t& other) {
    [[maybe_unused]] bool overflow = __builtin_add_overflow(integral, other, &integral);
    DEBUG_ASSERT(!overflow);
    return *this;
  }
  FractionalBytes& operator-=(const uint64_t& other) {
    [[maybe_unused]] bool overflow = __builtin_sub_overflow(integral, other, &integral);
    DEBUG_ASSERT(!overflow);
    return *this;
  }
  FractionalBytes& operator/=(const uint64_t& other) {
    // TODO(https://fxbug.dev/338300808): Remove this logic once we always generate fractional
    // bytes from attribution codepaths.
#if !ENABLE_LEGACY_ATTRIBUTION
    // Input fraction must always be <1 to guard against overflow.
    // If this is true, the sum of fractions must be <1:
    // The sum is:
    //  `(fractional / other) + (1 / other) * remainder`
    // which we can rewrite as
    //  `(fractional + remainder) / other`
    // We know that fractional < 1 and remainder < other, thus (fractional + remainder) < other and
    // the rewritten sum cannot be >=1.
    DEBUG_ASSERT(fractional < kOneByte);
    const uint64_t remainder = integral % other;
    const Fraction scaled_remainder = (kOneByte / other) * remainder;
    fractional = (fractional / other) + scaled_remainder;
    DEBUG_ASSERT(fractional < kOneByte);
#endif
    integral /= other;

    return *this;
  }

  FractionalBytes operator+(const FractionalBytes& other) const {
    FractionalBytes ret{*this};
    ret += other;
    return ret;
  }
  FractionalBytes& operator+=(const FractionalBytes& other) {
    // TODO(https://fxbug.dev/338300808): Remove this logic once we always generate fractional
    // bytes from attribution codepaths.
#if !ENABLE_LEGACY_ATTRIBUTION
    // Input fractions must always be <1 to guard against overflow.
    // If the fractional sum is >=1, then roll that overflow byte into the integral part.
    DEBUG_ASSERT(fractional < kOneByte);
    DEBUG_ASSERT(other.fractional < kOneByte);
    fractional += other.fractional;
    if (fractional >= kOneByte) {
      [[maybe_unused]] bool overflow = __builtin_add_overflow(integral, 1, &integral);
      DEBUG_ASSERT(!overflow);
      fractional -= kOneByte;
    }
#endif
    [[maybe_unused]] bool overflow = __builtin_add_overflow(integral, other.integral, &integral);
    DEBUG_ASSERT(!overflow);

    return *this;
  }

  bool operator==(const FractionalBytes& other) const {
    return integral == other.integral && fractional == other.fractional;
  }
  bool operator!=(const FractionalBytes& other) const { return !(*this == other); }

  size_t integral = 0;
  // Mark all fractional values as invalid when using legacy attribution - it can't generate valid
  // fractional values.
  // TODO(https://fxbug.dev/338300943): Initialize these to 0 once all code can generate valid
  // fractional values.
#if ENABLE_LEGACY_ATTRIBUTION
  constexpr static Fraction fractional = Fraction::Max();
#else
  Fraction fractional = Fraction::FromRaw(0);
#endif
};

// Structure to store counts of memory attributed to VMOs or portions thereof.
//
// These counts can be accumulated to support attributing memory across composite objects such as
// address spaces or processes.
//
// The `scaled_bytes` fields may contain a fractional number of bytes, and the structure stores the
// fractional counts in fixed point with 63 bits of precision.
struct AttributionCounts {
  size_t total_bytes() const { return uncompressed_bytes + compressed_bytes; }
  size_t total_private_bytes() const {
    return private_uncompressed_bytes + private_compressed_bytes;
  }
  FractionalBytes total_scaled_bytes() const {
    return scaled_uncompressed_bytes + scaled_compressed_bytes;
  }

  AttributionCounts& operator+=(const AttributionCounts& other) {
    uncompressed_bytes += other.uncompressed_bytes;
    compressed_bytes += other.compressed_bytes;
#if !ENABLE_LEGACY_ATTRIBUTION
    private_uncompressed_bytes += other.private_uncompressed_bytes;
    private_compressed_bytes += other.private_compressed_bytes;
    scaled_uncompressed_bytes += other.scaled_uncompressed_bytes;
    scaled_compressed_bytes += other.scaled_compressed_bytes;
#endif
    return *this;
  }

// TODO(https://fxbug.dev/338300943): While legacy attribution is supported the compiler will
// complain about comparing the same static constexpr to itself. Once legacy attribution is removed
// this warning disabling can also be removed.
#pragma GCC diagnostic push
#pragma GCC diagnostic ignored "-Wtautological-compare"
  bool operator==(const AttributionCounts& other) const {
    return uncompressed_bytes == other.uncompressed_bytes &&
           compressed_bytes == other.compressed_bytes &&
           private_uncompressed_bytes == other.private_uncompressed_bytes &&
           private_compressed_bytes == other.private_compressed_bytes &&
           scaled_uncompressed_bytes == other.scaled_uncompressed_bytes &&
           scaled_compressed_bytes == other.scaled_compressed_bytes;
  }
#pragma GCC diagnostic pop
  bool operator!=(const AttributionCounts& other) const { return !(*this == other); }

  size_t uncompressed_bytes = 0;
  size_t compressed_bytes = 0;
  // TODO(https://fxbug.dev/338300943): These are declared as static constexpr when using legacy
  // attribution to minimize the size of this object, which is included in the VmMapping and
  // VmCowPages objects.
#if ENABLE_LEGACY_ATTRIBUTION
  constexpr static size_t private_uncompressed_bytes = 0;
  constexpr static size_t private_compressed_bytes = 0;
  constexpr static FractionalBytes scaled_uncompressed_bytes = FractionalBytes(0u);
  constexpr static FractionalBytes scaled_compressed_bytes = FractionalBytes(0u);
#else
  size_t private_uncompressed_bytes = 0;
  size_t private_compressed_bytes = 0;
  FractionalBytes scaled_uncompressed_bytes;
  FractionalBytes scaled_compressed_bytes;
#endif
};

}  // namespace vm

#endif  // ZIRCON_KERNEL_VM_INCLUDE_VM_ATTRIBUTION_H_
