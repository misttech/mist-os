// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <sys/syscall.h>
#include <sys/time.h>
#include <zircon/compiler.h>

#include "vdso-calculate-time.h"
#include "vdso-common.h"
#include "vdso-platform.h"

// Zircon and its libraries do not support 32-bit architectures. As such, we
// bring all the necessary pieces in here to support the 32-bit VDSOs.
// The fasttime library above depends on an unstable ABI, so pulling it in here
// shouldn't change the exposure. We define internal time so that we don't
// require extensive support in Zircon for the arm target.
#include <lib/fasttime/internal/abi.h>

constexpr uint64_t kFasttimeVersion = 1;

inline zx_ticks_t get_raw_ticks() {
  // ARMv7 support a userland performance counter which Zircon enables if the
  // real ARM64 processor supports it (FEAT_PMUv3).  As such, we check for
  // both access and for the counter to be enabled before attempting to read
  // it.
  //
  uint32_t access_enabled;
  __asm__ volatile("mrc p15, 0, %0, c9, c14, 0" : "=r"(access_enabled));
  if (!(access_enabled & 1)) {
    return 0;
  }
  uint32_t counter_enabled;
  __asm__ volatile("mrc p15, 0, %0, c9, c12, 1" : "=r"(counter_enabled));
  if (!(counter_enabled & 0x80000000UL)) {
    return 0;
  }
  uint32_t counter_value;
  __asm__ volatile("mrc p15, 0, %0, c9, c13, 0" : "=r"(counter_value));
  return (counter_value << 6ULL);
}

int64_t calculate_monotonic_time_nsec() {
  fasttime::internal::TimeValues* tvalues =
      reinterpret_cast<fasttime::internal::TimeValues*>(&time_values);
  // The time values contain everything we need to go from raw ticks to
  // monotonic time.
  // 0. Ensure we are using the known version of the TimeValues ABI
  if (tvalues->version != kFasttimeVersion) {
    return ZX_TIME_INFINITE_PAST;
  }
  // 1. We adjust the raw ticks to monotonic ticks.
  const zx_ticks_t obs1 = tvalues->mono_ticks_offset.load(std::memory_order_relaxed);
  const zx_ticks_t raw_ticks = get_raw_ticks();
  const zx_ticks_t obs2 = tvalues->mono_ticks_offset.load(std::memory_order_relaxed);
  zx_ticks_t ticks = 0;
  if (raw_ticks != 0 && obs1 == obs2) {
    ticks = raw_ticks + obs1;
  } else {
    // Read failure.
    return ZX_TIME_INFINITE_PAST;
  }
  // 2. We adjust the monotonic ticks to the monotonic clock.
  //    lib/fasttime uses a more careful scaling function. We'll start a little
  //    clunkier.
  int64_t mtime = ticks / tvalues->ticks_to_time_denominator;
  mtime *= tvalues->ticks_to_time_numerator;
  return mtime;
}

int64_t calculate_boot_time_nsec() {
  fasttime::internal::TimeValues* tvalues =
      reinterpret_cast<fasttime::internal::TimeValues*>(&time_values);
  // 0. Ensure we have a known version of the time values
  if (tvalues->version != kFasttimeVersion) {
    return ZX_TIME_INFINITE_PAST;
  }
  // 1. Compute the boot ticks from raw ticks
  zx_ticks_t ticks = get_raw_ticks() + tvalues->boot_ticks_offset;

  // 2. We adjust as above
  //    lib/fasttime uses a more careful scaling function. We'll start a little
  //    clunkier.
  int64_t mtime = ticks / tvalues->ticks_to_time_denominator;
  mtime *= tvalues->ticks_to_time_numerator;
  return mtime;
}

static uint64_t udiv64(uint64_t dividend, uint64_t divisor, uint64_t* remainder) {
  uint64_t quotient = 0;
  uint32_t count = 1;

  // Shortcut special cases
  if (divisor == 0) {
    // div-by-0.
    return UINT64_MAX;
  }
  if (divisor > dividend) {
    *remainder = dividend;
    return 0;
  }
  if (divisor == dividend) {
    *remainder = 0;
    return 1;
  }

  // If not, we want to move the divisor as far left as we can,
  // and then compare against an accumulator of the dividend's left
  // bits. If the accumulator is larger, then we subtract it out, set
  // the quotient bit and keep going.  The quotient bit can be set
  // and shifted because it can't be larger than the divisor was.
  *remainder = 0;

  // Find the first bit in the divisor and shift it left,
  // so we can test at each ste.
  while ((divisor >> 63) == 0) {
    count++;
    divisor <<= 1;
  }
  *remainder = dividend;
  while (count) {
    quotient <<= 1;  // shift here so our last bit is available.
    if (*remainder >= divisor) {
      quotient |= 1;
      *remainder -= divisor;
    }
    count -= 1;
    divisor >>= 1;
  }
  return quotient;
}

static int64_t div64(int64_t dividend, int64_t divisor, int64_t* remainder) {
  uint64_t udividend;
  uint64_t udivisor;
  uint64_t uremainder = 0;
  uint64_t uquotient;
  int q_negate = 1;
  int r_negate = 1;
  if (divisor < 0) {
    udivisor = divisor * -1;
    q_negate *= -1;
  } else {
    udivisor = divisor;
  }
  if (dividend < 0) {
    udividend = dividend * -1;
    q_negate *= -1;
    r_negate *= -1;
  } else {
    udividend = dividend;
  }
  uquotient = udiv64(udividend, udivisor, &uremainder);
  *remainder = uremainder * r_negate;
  return uquotient * q_negate;
}

extern "C" unsigned long long __aeabi_uidiv(unsigned long long numerator,
                                            unsigned long long denominator) {
  uint64_t r = 0;
  return udiv64(numerator, denominator, &r);
}

struct uldivmod_result {
  uint64_t q, r;
};
extern "C" struct uldivmod_result __aeabi_uldivmod(uint64_t numerator, uint64_t denominator) {
  struct uldivmod_result result;
  result.q = udiv64(numerator, denominator, &result.r);
  return result;
}

extern "C" long long __aeabi_idiv(long long numerator, long long denominator) {
  int64_t r = 0;
  return div64(numerator, denominator, &r);
}

struct ldivmod_result {
  int64_t quot, rem;
};
extern "C" struct ldivmod_result __aeabi_ldivmod(int64_t numerator, int64_t denominator) {
  int64_t r, q;
  q = div64(numerator, denominator, &r);
  return {q, r};
}

int syscall(intptr_t syscall_number, intptr_t arg1, intptr_t arg2, intptr_t arg3) {
  register intptr_t x0 asm("r0") = arg1;
  register intptr_t x1 asm("r1") = arg2;
  register intptr_t x2 asm("r2") = arg3;
  register intptr_t number asm("r7") = syscall_number;

  __asm__ volatile("svc #0" : "=r"(x0) : "0"(x0), "r"(x1), "r"(x2), "r"(number) : "memory");
  return static_cast<int>(x0);
}

extern "C" __EXPORT __attribute__((naked)) void __kernel_rt_sigreturn() {
  __asm__ volatile("mov r7, %0" ::"I"(__NR_rt_sigreturn));
  __asm__ volatile("svc #0");
}

extern "C" __EXPORT int __kernel_clock_gettime(int clock_id, struct timespec* tp) {
  return clock_gettime_impl(clock_id, tp);
}

extern "C" __EXPORT int __kernel_clock_getres(int clock_id, struct timespec* tp) {
  return clock_getres_impl(clock_id, tp);
}

extern "C" __EXPORT int __kernel_gettimeofday(struct timeval* tv, struct timezone* tz) {
  return gettimeofday_impl(tv, tz);
}
