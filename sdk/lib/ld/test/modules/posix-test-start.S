// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/arch/asm.h>

// This defines the entry point in assembly, such that it calls:
//
//   extern "C" int64_t TestStart(...);
//
// In the in-process version, that's a tail call so it returns directly to the
// in-process caller.  In the separate-process version, the return value is
// passed to _exit.

.function _start, global

  // This can assume the sp is already aligned to 16 by the kernel.

#if defined(__aarch64__)

  mov x0, sp
#ifdef IN_PROCESS_TEST
  b TestStart
#else
  bl TestStart
  // Return value in x0 is argument.
  bl _exit
  udf #0
#endif

#elif defined(__riscv)

  mv a0, sp
#ifdef IN_PROCESS_TEST
  b TestStart
#else
  call TestStart
  // Return value in a0 is argument.
  call _exit
  unimp
#endif

#elif defined(__x86_64__)

  mov %rsp, %rdi
  // If called by ld-startup-tests.cc code, the return address is already in
  // the word just below %rsp.  If started otherwise, that word will be zero.
  // Either way %rsp needs to be adjusted down to 8 mod 16 as a call would do
  // by pushing the return address for the C calling convention.  Instead of a
  // call followed by a trap if it ever did return, just adjust %rsp down and
  // jump so that either an original return address or a fault-inducing zero is
  // the callee's return address.
  sub $8, %rsp
#ifdef IN_PROCESS_TEST
  jmp TestStart
#else
  call TestStart
  // Move the return value into the argument register.
  mov %rax, %rdi
  call _exit
  ud2
#endif

#else
#error "unsupported machine"
#endif

.end_function
