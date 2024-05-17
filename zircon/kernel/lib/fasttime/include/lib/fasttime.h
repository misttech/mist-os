// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_FASTTIME_INCLUDE_LIB_FASTTIME_H_
#define ZIRCON_KERNEL_LIB_FASTTIME_INCLUDE_LIB_FASTTIME_H_

#include <lib/affine/ratio.h>
#include <zircon/time.h>
#include <zircon/types.h>

#ifdef __x86_64__
#include <x86intrin.h>
#endif

#include "lib/time-values-abi.h"

namespace internal {

#if __aarch64__
inline zx_ticks_t get_raw_ticks(const TimeValues& tvalues) {
  if (tvalues.use_a73_errata_mitigation) {
    return get_raw_ticks_arm_a73();
  }
  // Read the virtual counter.
  zx_ticks_t ticks;
  __asm__ volatile("mrs %0, cntvct_el0" : "=r"(ticks));
  return ticks;
}

inline zx_ticks_t get_raw_ticks_arm_a73() {
  zx_ticks_t ticks1, ticks2;
  __asm__ volatile("mrs %0, cntvct_el0" : "=r"(ticks1));
  __asm__ volatile("mrs %0, cntvct_el0" : "=r"(ticks2));
  return (((ticks1 ^ ticks2) >> 32) & 1) ? ticks1 : ticks2;
}

#elif defined(__x86_64__)
inline zx_ticks_t get_raw_ticks(const TimeValues& tvalues) { return __rdtsc(); }

#elif defined(__riscv)

inline zx_ticks_t get_raw_ticks(const TimeValues& tvalues) {
  zx_ticks_t ticks;
  __asm__ volatile("rdtime %0" : "=r"(ticks));
  return ticks;
}

#else

#error Unsupported architecture

#endif

constexpr uint64_t kFasttimeVersion = 1;

enum class FasttimeVerificationMode : uint8_t {
  kNormal,
  kSkip,
};

inline bool check_fasttime_version(const TimeValues& tvalues) {
  return tvalues.version == kFasttimeVersion;
}

template <FasttimeVerificationMode kVerificationMode = FasttimeVerificationMode::kNormal>
inline zx_ticks_t compute_monotonic_ticks(const TimeValues& tvalues) {
  if constexpr (kVerificationMode == FasttimeVerificationMode::kNormal) {
    if (!tvalues.usermode_can_access_ticks || tvalues.version != kFasttimeVersion) {
      return ZX_TIME_INFINITE_PAST;
    }
  }
  return internal::get_raw_ticks(tvalues) + tvalues.raw_ticks_to_ticks_offset;
}

template <FasttimeVerificationMode kVerificationMode = FasttimeVerificationMode::kNormal>
inline zx_time_t compute_monotonic_time(const TimeValues& tvalues) {
  const zx_ticks_t ticks = compute_monotonic_ticks<kVerificationMode>(tvalues);
  if constexpr (kVerificationMode == FasttimeVerificationMode::kNormal) {
    if (ticks == ZX_TIME_INFINITE_PAST) {
      return ticks;
    }
  }
  const affine::Ratio ticks_to_mono_ratio(tvalues.ticks_to_mono_numerator,
                                          tvalues.ticks_to_mono_denominator);
  return ticks_to_mono_ratio.Scale(ticks);
}

}  // namespace internal

namespace fasttime {

inline zx_ticks_t compute_monotonic_ticks(zx_vaddr_t time_values_addr) {
  const internal::TimeValues* time_values =
      reinterpret_cast<internal::TimeValues*>(time_values_addr);
  return internal::compute_monotonic_ticks<internal::FasttimeVerificationMode::kNormal>(
      *time_values);
}

inline zx_time_t compute_monotonic_time(zx_vaddr_t time_values_addr) {
  const internal::TimeValues* time_values =
      reinterpret_cast<internal::TimeValues*>(time_values_addr);
  return internal::compute_monotonic_time<internal::FasttimeVerificationMode::kNormal>(
      *time_values);
}

}  // namespace fasttime

#endif  // ZIRCON_KERNEL_LIB_FASTTIME_INCLUDE_LIB_FASTTIME_H_
