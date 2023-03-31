// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#define EXPORT __attribute__((visibility("default")))
#define WEAK __attribute__((weak, visibility("default")))

#include <inttypes.h>
#include <sys/syscall.h>
#include <sys/time.h>
#include <time.h>

extern "C" int syscall(intptr_t syscall_number, intptr_t arg1, intptr_t arg2, intptr_t arg3) {
  int ret;
  __asm__ volatile("syscall;"
                   : "=a"(ret)
                   : "a"(syscall_number), "D"(arg1), "S"(arg2), "d"(arg3)
                   : "rcx", "r11", "memory");
  return ret;
}

extern "C" EXPORT int __vdso_clock_gettime(int clock_id, struct timespec* tp) {
  int ret = syscall(__NR_clock_gettime, static_cast<intptr_t>(clock_id),
                    reinterpret_cast<intptr_t>(tp), 0);
  return ret;
}

extern "C" WEAK int clock_gettime(int clock_id, struct timespec* tp)
    __attribute__((alias("__vdso_clock_gettime")));

extern "C" EXPORT int __vdso_clock_getres(int clock_id, struct timespec* tp) {
  int ret = syscall(__NR_clock_getres, static_cast<intptr_t>(clock_id),
                    reinterpret_cast<intptr_t>(tp), 0);
  return ret;
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
