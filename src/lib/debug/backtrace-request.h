// Copyright 2016 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_LIB_DEBUG_BACKTRACE_REQUEST_H_
#define SRC_LIB_DEBUG_BACKTRACE_REQUEST_H_

#include <stdint.h>
#include <zircon/compiler.h>

// Special values we put in the first register to let the exception handler know
// that we are just requesting a backtrace and we should resume the thread.
#define BACKTRACE_REQUEST_MAGIC_ALL_THREADS ((uint64_t)0xee726573756d65ee)
#define BACKTRACE_REQUEST_MAGIC_CURRENT_THREAD ((uint64_t)0xee726573756d65ef)

// Prints backtrace for all threads and resumes without killing the process.
__ALWAYS_INLINE static inline void backtrace_request_all_threads(void) {
  // We set a software breakpoint to trigger the exception handling in
  // crashsvc, which will print the debug info, including the backtrace.
  //
  // We write the "magic" value in the first register (rax on x64, x0 on arm64) so that the
  // exception handler can check for it and resume the thread rather than kill the process.
#ifdef __x86_64__
  __asm__("int3" : : "a"(BACKTRACE_REQUEST_MAGIC_ALL_THREADS));
#endif
#ifdef __aarch64__
  // This is what gdb uses.
  __asm__(
      "mov x0, %0\n\t"
      "brk 0"
      :
      : "r"(BACKTRACE_REQUEST_MAGIC_ALL_THREADS)
      : "x0");
#endif
}

// Requests a backtrace only for the current thread.
__ALWAYS_INLINE static inline void backtrace_request_current_thread(void) {
  // Put 1 in rdi / x1.
#ifdef __x86_64__
  __asm__("int3" : : "a"(BACKTRACE_REQUEST_MAGIC_CURRENT_THREAD));
#endif
#ifdef __aarch64__
  // This is what gdb uses.
  __asm__(
      "mov x0, %0\n\t"
      "brk 0"
      :
      : "r"(BACKTRACE_REQUEST_MAGIC_CURRENT_THREAD)
      : "x0");
#endif
}

#endif  // SRC_LIB_DEBUG_BACKTRACE_REQUEST_H_
