// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "vdso-common.h"

#include <lib/fasttime/time.h>
#include <sys/syscall.h>

#include "vdso-calculate-time.h"
#include "vdso-platform.h"

int64_t calculate_monotonic_time_nsec() {
  zx_vaddr_t time_values_addr = reinterpret_cast<zx_vaddr_t>(&time_values);
  return fasttime::compute_monotonic_time(time_values_addr);
}

int64_t calculate_boot_time_nsec() {
  zx_vaddr_t time_values_addr = reinterpret_cast<zx_vaddr_t>(&time_values);
  return fasttime::compute_boot_time(time_values_addr);
}

int clock_gettime_impl(int clock_id, timespec* tp) {
  int ret = 0;
  if ((clock_id == CLOCK_MONOTONIC) || (clock_id == CLOCK_MONOTONIC_RAW) ||
      (clock_id == CLOCK_MONOTONIC_COARSE)) {
    int64_t monot_nsec = calculate_monotonic_time_nsec();
    if (monot_nsec == ZX_TIME_INFINITE_PAST) {
      // If calculate_monotonic_time_nsec returned ZX_TIME_INFINITE_PAST, then either:
      // 1. The ticks register is not userspace accessible, or
      // 2. The version of libfasttime is out of sync with the version of the time values VMO.
      // In either case, we must invoke a syscall to get the correct time value.
      ret = syscall(__NR_clock_gettime, static_cast<intptr_t>(clock_id),
                    reinterpret_cast<intptr_t>(tp), 0);
      return ret;
    }
    tp->tv_sec = monot_nsec / kNanosecondsPerSecond;
    tp->tv_nsec = monot_nsec % kNanosecondsPerSecond;
  } else if (clock_id == CLOCK_BOOTTIME) {
    int64_t boot_nsec = calculate_boot_time_nsec();
    if (boot_nsec == ZX_TIME_INFINITE_PAST) {
      // Once again, if calculate_boot_time_nsec returned ZX_TIME_INFINITE_PAST, then either:
      // 1. The ticks register is not userspace accessible, or
      // 2. The version of libfasttime is out of sync with the version of the time values VMO.
      // In either case, we must invoke a syscall to get the correct time value.
      ret = syscall(__NR_clock_gettime, static_cast<intptr_t>(clock_id),
                    reinterpret_cast<intptr_t>(tp), 0);
      return ret;
    }
    tp->tv_sec = boot_nsec / kNanosecondsPerSecond;
    tp->tv_nsec = boot_nsec % kNanosecondsPerSecond;
  } else if (clock_id == CLOCK_REALTIME) {
    uint64_t utc_nsec = calculate_utc_time_nsec();
    if (utc_nsec == kUtcInvalid) {
      // The syscall is used instead of endlessly retrying to acquire the seqlock. This gives the
      // writer thread of the seqlock a chance to run, even if it happens to have a lower priority
      // than the current thread.
      ret = syscall(__NR_clock_gettime, static_cast<intptr_t>(clock_id),
                    reinterpret_cast<intptr_t>(tp), 0);
      return ret;
    }
    tp->tv_sec = utc_nsec / kNanosecondsPerSecond;
    tp->tv_nsec = utc_nsec % kNanosecondsPerSecond;
  } else {
    ret = syscall(__NR_clock_gettime, static_cast<intptr_t>(clock_id),
                  reinterpret_cast<intptr_t>(tp), 0);
  }
  return ret;
}

bool is_valid_cpu_clock(int clock_id) { return (clock_id & 7) != 7 && (clock_id & 3) < 3; }

int clock_getres_impl(int clock_id, timespec* tp) {
  if (clock_id < 0 && !is_valid_cpu_clock(clock_id)) {
    return -EINVAL;
  }
  if (tp == nullptr) {
    return 0;
  }
  switch (clock_id) {
    case CLOCK_REALTIME:
    case CLOCK_REALTIME_ALARM:
    case CLOCK_REALTIME_COARSE:
    case CLOCK_MONOTONIC:
    case CLOCK_MONOTONIC_COARSE:
    case CLOCK_MONOTONIC_RAW:
    case CLOCK_BOOTTIME:
    case CLOCK_BOOTTIME_ALARM:
    case CLOCK_THREAD_CPUTIME_ID:
    case CLOCK_PROCESS_CPUTIME_ID:
      tp->tv_sec = 0;
      tp->tv_nsec = 1;
      return 0;

    default:
      int ret = syscall(__NR_clock_getres, static_cast<intptr_t>(clock_id),
                        reinterpret_cast<intptr_t>(tp), 0);
      return ret;
  }
}

int gettimeofday_impl(timeval* tv, struct timezone* tz) {
  if (tz != nullptr) {
    int ret = syscall(__NR_gettimeofday, reinterpret_cast<intptr_t>(tv),
                      reinterpret_cast<intptr_t>(tz), 0);
    return ret;
  }
  if (tv == nullptr) {
    return 0;
  }
  int64_t utc_nsec = calculate_utc_time_nsec();
  if (utc_nsec != kUtcInvalid) {
    tv->tv_sec = utc_nsec / kNanosecondsPerSecond;
    tv->tv_usec = (utc_nsec % kNanosecondsPerSecond) / 1'000;
    return 0;
  }
  // The syscall is used instead of endlessly retrying to acquire the seqlock. This gives the
  // writer thread of the seqlock a chance to run, even if it happens to have a lower priority
  // than the current thread.
  int ret =
      syscall(__NR_gettimeofday, reinterpret_cast<intptr_t>(tv), reinterpret_cast<intptr_t>(tz), 0);
  return ret;
}
