// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/arch/asm.h>

// This defines the entry point in assembly, such that it calls:
//   extern "C" StartLdResult StartLd(StartupStack& stack);
// And then jumps to the user entry point in the return value, with
// the original stack intact.
// (See posix-startup.cc for definition.)

.function _start, global

  // This can assume the sp is already aligned to 16 by the kernel.

#if defined(__aarch64__)

  mov x0, sp
  b TestStart

#elif defined(__riscv)

  mv a0, sp
  b TestStart

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
  jmp TestStart

#else
#error "unsupported machine"
#endif

.end_function
