// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed
// by a BSD-style license that can be found in the LICENSE file.

#ifndef ZIRCON_KERNEL_LIB_FASTTIME_INCLUDE_LIB_FASTTIME_INTERNAL_TIME_H_
#define ZIRCON_KERNEL_LIB_FASTTIME_INCLUDE_LIB_FASTTIME_INTERNAL_TIME_H_

#include <lib/affine/ratio.h>
#include <lib/arch/intrin.h>
#include <lib/fasttime/internal/abi.h>
#include <zircon/time.h>
#include <zircon/types.h>

namespace fasttime::internal {

#if defined(__aarch64__)

#define TIMER_REG_CNTVCT "cntvct_el0"

inline zx_ticks_t get_raw_ticks_arm_a73() {
  zx_ticks_t ticks1 = __arm_rsr64(TIMER_REG_CNTVCT);
  zx_ticks_t ticks2 = __arm_rsr64(TIMER_REG_CNTVCT);
  return (((ticks1 ^ ticks2) >> 32) & 1) ? ticks1 : ticks2;
}

inline zx_ticks_t get_raw_ticks(const TimeValues& tvalues) {
  if (tvalues.use_a73_errata_mitigation) {
    return get_raw_ticks_arm_a73();
  }
  return __arm_rsr64(TIMER_REG_CNTVCT);
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

}  // namespace fasttime::internal

#endif  // ZIRCON_KERNEL_LIB_FASTTIME_INCLUDE_LIB_FASTTIME_INTERNAL_TIME_H_
