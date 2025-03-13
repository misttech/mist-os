// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/arch/asm.h>
#include <zircon/tls.h>

#include "asm-linkage.h"
#include "../jmp_buf.h"


.macro mangle index, reg
  xor LIBC_ASM_LINKAGE(gJmpBufManglers)+8*\index(%rip), \reg
.endm

jmp_buf.llvm_libc_function setjmp

  // Copy the values that need mangling into temporaries.
  mov (%rsp), %rax                            // PC
  mov %rsp, %rcx                              // SP
  mov %rbp, %rdx                              // FP
  mov %fs:ZX_TLS_UNSAFE_SP_OFFSET, %rsi       // Unsafe SP

  // Store all the vanilla callee-saves registers.
  mov %rbx, 8*JB_RBX(%rdi)
  mov %r12, 8*JB_R12(%rdi)
  mov %r13, 8*JB_R13(%rdi)
  mov %r14, 8*JB_R14(%rdi)
  mov %r15, 8*JB_R15(%rdi)

  // Mangle each temporary.
  mangle JB_PC, %rax
  mangle JB_SP, %rcx
  mangle JB_FP, %rdx
  mangle JB_USP, %rsi

  // Store the mangled values.
  mov %rax, 8*JB_PC(%rdi)
  mov %rcx, 8*JB_SP(%rdi)
  mov %rdx, 8*JB_FP(%rdi)
  mov %rsi, 8*JB_USP(%rdi)

  // %rax = 0
  xor %eax, %eax

  ret

.end_function
