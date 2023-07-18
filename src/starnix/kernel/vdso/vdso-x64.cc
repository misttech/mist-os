// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <cerrno>
#define EXPORT __attribute__((visibility("default")))
#define WEAK __attribute__((weak, visibility("default")))
#define __LOCAL __attribute__((__visibility__("hidden")))

#include <inttypes.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>

#include "vdso-constants.h"

extern __LOCAL struct vdso_constants DATA_CONSTANTS;

extern "C" int syscall(intptr_t syscall_number, intptr_t arg1, intptr_t arg2, intptr_t arg3) {
  int ret;
  __asm__ volatile("syscall;"
                   : "=a"(ret)
                   : "a"(syscall_number), "D"(arg1), "S"(arg2), "d"(arg3)
                   : "rcx", "r11", "memory");
  return ret;
}

extern "C" EXPORT int __vdso_clock_gettime(int clock_id, struct timespec* tp) {
  int ret = 0;
  const int64_t NSEC_PER_SEC = 1'000'000'000;
  if ((clock_id == CLOCK_MONOTONIC) || (clock_id == CLOCK_MONOTONIC_RAW) ||
      (clock_id == CLOCK_MONOTONIC_COARSE) || (clock_id == CLOCK_BOOTTIME)) {
    uint64_t raw_ticks = __rdtsc();
    uint64_t ticks = raw_ticks + DATA_CONSTANTS.raw_ticks_to_ticks_offset;
    // TODO(mariagl): This could potentially overflow; Find a way to avoid this.
    uint64_t monot_nsec =
        ticks * DATA_CONSTANTS.ticks_to_mono_numerator / DATA_CONSTANTS.ticks_to_mono_denominator;
    tp->tv_sec = monot_nsec / NSEC_PER_SEC;
    tp->tv_nsec = monot_nsec % NSEC_PER_SEC;
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
