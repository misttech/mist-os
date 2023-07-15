// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/arch/asm.h>

// This defines the entry point in assembly, such that it calls:
//   extern "C" uintptr_t void StartLd(StartupStack& stack);
// And then jumps to the user entry point at the return value, with
// the original stack intact.
// (See posix-startup.cc for definition.)

// TODO(mcgrathr): for now, it instead just returns what StartLd returns.

.function _start, global

  // This can assume the sp is already aligned to 16 by the kernel.

#if defined(__aarch64__)

  mov x0, sp
  bl StartLd
  ret // br x0

#elif defined(__riscv)

  mv a0, sp
  call StartLd
  ret // jr a0

#elif defined(__x86_64__)

  // The ld-startup-tests.cc code puts its return address here.
  // Save it in a call-saved register.
  // TODO(mcgrathr): later we can keep this bit when we hand off to
  // the executable, so test executables can know this return protocol
  mov -8(%rsp), %rbx

  mov %rsp, %rdi
  call StartLd
  jmp *%rbx // jmp *%rax

#else
#error "unsupported machine"
#endif

.end_function
