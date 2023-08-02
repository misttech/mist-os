// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/arch/asm.h>

// This defines the entry point in assembly, such that it calls:
//   extern "C" StartLdResult StartLd(...);
// The arguments are whatever the arguments to _start were.
// The return value is two words: first the argument to pass to the
// user entry point, and then the user entry point PC.

.function _start, global

  // This can assume the sp is already aligned to 16 by the kernel.

#if defined(__aarch64__)

  .prologue.fp
  bl StartLd
  .epilogue.fp

  // The argument is already in x0.
  br x1

#elif defined(__riscv)

  .prologue.fp
  call StartLd
  .epilogue.fp

  // The argument is already in a0.
  jr a1

#elif defined(__x86_64__)

  .prologue.fp
  call StartLd
  .epilogue.fp

  // Move the argument into place.
  mov %rax, %rdi
  jmp *%rdx

#else
#error "unsupported machine"
#endif

.end_function
