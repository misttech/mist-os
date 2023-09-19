// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <atomic>
#include <cerrno>
#define EXPORT __attribute__((visibility("default")))
#define WEAK __attribute__((weak, visibility("default")))
#define __LOCAL __attribute__((__visibility__("hidden")))

#include <inttypes.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>

#include "src/starnix/kernel/vdso/vvar-data.h"

extern "C" vvar_data vvar;

const int64_t NSEC_PER_SEC = 1'000'000'000;
const int64_t UTC_INVALID = 0;

extern "C" int syscall(intptr_t syscall_number, intptr_t arg1, intptr_t arg2, intptr_t arg3) {
  int ret;
  __asm__ volatile("syscall;"
                   : "=a"(ret)
                   : "a"(syscall_number), "D"(arg1), "S"(arg2), "d"(arg3)
                   : "rcx", "r11", "memory");
  return ret;
}

inline int64_t calculate_monotonic_time_nsec() {
  uint64_t raw_ticks = __rdtsc();
  uint64_t ticks = raw_ticks + vvar.raw_ticks_to_ticks_offset.load(std::memory_order_acquire);
  // TODO(mariagl): This could potentially overflow; Find a way to avoid this.
  uint64_t monot_nsec = ticks * vvar.ticks_to_mono_numerator.load(std::memory_order_acquire) /
                        vvar.ticks_to_mono_denominator.load(std::memory_order_acquire);
  return monot_nsec;
}

inline int64_t calculate_utc_time_nsec() {
  int64_t monotonic_time = calculate_monotonic_time_nsec();

  // Mono to utc transform is read from vvar_data. The data is protected by a seqlock, and so
  // a seqlock reader is implemented
  // Check that the state of the seqlock shows that the transform is not being updated
  uint64_t seq_num1 = vvar.seq_num.load(std::memory_order_acquire);
  if (seq_num1 & 1) {
    // Cannot read, because a write is in progress
    return UTC_INVALID;
  }
  int64_t mono_to_utc_reference_offset =
      vvar.mono_to_utc_reference_offset.load(std::memory_order_acquire);
  int64_t mono_to_utc_synthetic_offset =
      vvar.mono_to_utc_synthetic_offset.load(std::memory_order_acquire);
  uint32_t mono_to_utc_reference_ticks =
      vvar.mono_to_utc_reference_ticks.load(std::memory_order_acquire);
  uint32_t mono_to_utc_synthetic_ticks =
      vvar.mono_to_utc_synthetic_ticks.load(std::memory_order_acquire);
  // Check that the state of the seqlock has not changed while reading the transform
  uint64_t seq_num2 = vvar.seq_num.load(std::memory_order_acquire);
  if (seq_num1 != seq_num2) {
    // Data has been updated during the reading of it, so is invalid
    return UTC_INVALID;
  }
  int64_t utc_nsec = (monotonic_time - mono_to_utc_reference_offset) * mono_to_utc_synthetic_ticks /
                         mono_to_utc_reference_ticks +
                     mono_to_utc_synthetic_offset;
  return utc_nsec;
}

extern "C" EXPORT int __vdso_clock_gettime(int clock_id, struct timespec* tp) {
  int ret = 0;
  if ((clock_id == CLOCK_MONOTONIC) || (clock_id == CLOCK_MONOTONIC_RAW) ||
      (clock_id == CLOCK_MONOTONIC_COARSE) || (clock_id == CLOCK_BOOTTIME)) {
    uint64_t monot_nsec = calculate_monotonic_time_nsec();
    tp->tv_sec = monot_nsec / NSEC_PER_SEC;
    tp->tv_nsec = monot_nsec % NSEC_PER_SEC;
  } else if (clock_id == CLOCK_REALTIME) {
    uint64_t utc_nsec = calculate_utc_time_nsec();
    if (utc_nsec == UTC_INVALID) {
      // The syscall is used instead of endlessly retrying to acquire the seqlock. This gives the
      // writer thread of the seqlock a chance to run, even if it happens to have a lower priority
      // than the current thread.
      ret = syscall(__NR_clock_gettime, static_cast<intptr_t>(clock_id),
                    reinterpret_cast<intptr_t>(tp), 0);
      return ret;
    }
    tp->tv_sec = utc_nsec / NSEC_PER_SEC;
    tp->tv_nsec = utc_nsec % NSEC_PER_SEC;
  } else {
    ret = syscall(__NR_clock_gettime, static_cast<intptr_t>(clock_id),
                  reinterpret_cast<intptr_t>(tp), 0);
  }
  return ret;
}

extern "C" WEAK int clock_gettime(int clock_id, struct timespec* tp)
    __attribute__((alias("__vdso_clock_gettime")));

bool is_valid_cpu_clock(int clock_id) { return (clock_id & 7) != 7 && (clock_id & 3) < 3; }

extern "C" EXPORT int __vdso_clock_getres(int clock_id, struct timespec* tp) {
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

extern "C" WEAK int clock_getres(int clock_id, struct timespec* tp)
    __attribute__((alias("__vdso_clock_getres")));

extern "C" EXPORT int __vdso_getcpu(unsigned* cpu, void* cache, void* unused) {
  int ret = syscall(__NR_getcpu, reinterpret_cast<intptr_t>(cpu), reinterpret_cast<intptr_t>(cache),
                    reinterpret_cast<intptr_t>(unused));
  return ret;
}

extern "C" WEAK int getcpu(unsigned* cpu, void* cache, void* unused)
    __attribute__((alias("__vdso_getcpu")));

extern "C" EXPORT int __vdso_gettimeofday(struct timeval* tv, struct timezone* tz) {
  if (tz != nullptr) {
    int ret = syscall(__NR_gettimeofday, reinterpret_cast<intptr_t>(tv),
                      reinterpret_cast<intptr_t>(tz), 0);
    return ret;
  }
  if (tv == nullptr) {
    return 0;
  }
  int64_t utc_nsec = calculate_utc_time_nsec();
  if (utc_nsec != UTC_INVALID) {
    tv->tv_sec = utc_nsec / NSEC_PER_SEC;
    tv->tv_usec = (utc_nsec % NSEC_PER_SEC) / 1'000;
    return 0;
  }
  // The syscall is used instead of endlessly retrying to acquire the seqlock. This gives the
  // writer thread of the seqlock a chance to run, even if it happens to have a lower priority
  // than the current thread.
  int ret =
      syscall(__NR_gettimeofday, reinterpret_cast<intptr_t>(tv), reinterpret_cast<intptr_t>(tz), 0);
  return ret;
}

extern "C" WEAK int gettimeofday(struct timeval* tv, struct timezone* tz)
    __attribute__((alias("__vdso_gettimeofday")));

extern "C" EXPORT time_t __vdso_time(time_t* t) {
  int ret = syscall(__NR_time, reinterpret_cast<intptr_t>(t), 0, 0);
  return ret;
}

extern "C" WEAK time_t time(time_t* t) __attribute__((alias("__vdso_time")));
