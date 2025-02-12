// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/arch/asm.h>
#include <zircon/tls.h>

#include "../jmp_buf.h"

jmp_buf.llvm_libc_function setjmp

#ifndef __riscv_float_abi_soft
  // Store call-saved FP registers.
  fsd fs0, JB_FS(0)*8(a0)
  fsd fs1, JB_FS(1)*8(a0)
  fsd fs2, JB_FS(2)*8(a0)
  fsd fs3, JB_FS(3)*8(a0)
  fsd fs4, JB_FS(4)*8(a0)
  fsd fs5, JB_FS(5)*8(a0)
  fsd fs6, JB_FS(6)*8(a0)
  fsd fs7, JB_FS(7)*8(a0)
  fsd fs8, JB_FS(8)*8(a0)
  fsd fs9, JB_FS(9)*8(a0)
  fsd fs10, JB_FS(10)*8(a0)
  fsd fs11, JB_FS(11)*8(a0)
#endif

  // Load the manglers into registers, as well as the unsafe SP.
  lla a1, LIBC_ASM_LINKAGE(gJmpBufManglers)
  ld a7, ZX_TLS_UNSAFE_SP_OFFSET(tp)
  ld a2, JB_PC*8(a1)
  ld a3, JB_SP*8(a1)
  ld a4, JB_FP*8(a1)
  ld a5, JB_USP*8(a1)
  ld a6, JB_SCSP*8(a1)

  // Store the vanilla call-saved registers.
  sd s1, JB_S(1)*8(a0)
  sd s2, JB_S(2)*8(a0)
  sd s3, JB_S(3)*8(a0)
  sd s4, JB_S(4)*8(a0)
  sd s5, JB_S(5)*8(a0)
  sd s6, JB_S(6)*8(a0)
  sd s7, JB_S(7)*8(a0)
  sd s8, JB_S(8)*8(a0)
  sd s9, JB_S(9)*8(a0)
  sd s10, JB_S(10)*8(a0)
  sd s11, JB_S(11)*8(a0)

  xor a2, a2, ra  // Mangled PC (ra).
  xor a3, a3, sp  // Mangled SP.
  xor a4, a4, s0  // Mangled FP (s0).
  xor a5, a5, a7  // Mangled unsafe SP.
  xor a6, a6, gp  // Mangled shadow call stack pointer (gp).

  sd a2, JB_PC*8(a0)
  sd a3, JB_SP*8(a0)
  sd a4, JB_FP*8(a0)
  sd a5, JB_USP*8(a0)
  sd a6, JB_SCSP*8(a0)

  mv a0, zero
  ret

.end_function
