// Copyright 2017 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSTEM_ULIB_C_SETJMP_FUCHSIA_JMP_BUF_H_
#define ZIRCON_SYSTEM_ULIB_C_SETJMP_FUCHSIA_JMP_BUF_H_

#include "asm-linkage.h"

// These get mangled so the raw pointer values don't leak into the heap.
#define JB_PC 0
#define JB_SP 1
#define JB_FP 2
#define JB_USP 3
#define JB_MANGLE_COUNT (4 + JB_ARCH_MANGLE_COUNT)

#ifdef __x86_64__

// Other callee-saves registers.
#define JB_RBX (JB_MANGLE_COUNT + 0)
#define JB_R12 (JB_MANGLE_COUNT + 1)
#define JB_R13 (JB_MANGLE_COUNT + 2)
#define JB_R14 (JB_MANGLE_COUNT + 3)
#define JB_R15 (JB_MANGLE_COUNT + 4)
#define JB_COUNT (JB_MANGLE_COUNT + 5)

#define JB_ARCH_MANGLE_COUNT 0

#elif defined(__aarch64__)

// The shadow call stack pointer (x18) is also mangled.
#define JB_ARCH_MANGLE_COUNT 1

// Callee-saves registers are [x19,x28] and [d8,d15].
#define JB_X(n) (JB_MANGLE_COUNT + n - 19)
#define JB_D(n) (JB_X(29) + n - 8)
#define JB_COUNT JB_D(16)

#elif defined(__riscv)

// The shadow call stack pointer (gp / x3) is also mangled.
#define JB_SCSP 4
#define JB_ARCH_MANGLE_COUNT 1

// Callee-saves registers are s0..s11, but s0 is FP and so handled above.
#define JB_S(n) (JB_MANGLE_COUNT + (n) - 1)

// FP registers fs0..fs11 are also callee-saves.
#define JB_FS(n) (JB_S(12) + (n))
#if JB_FS(0) <= JB_S(11)
#error "JB_FS defined wrong"
#endif

#define JB_COUNT JB_FS(12)

#else

#error what architecture?

#endif

#ifndef __ASSEMBLER__

#include <setjmp.h>

#include <array>
#include <cstdint>

namespace LIBC_NAMESPACE_DECL {

// This is used by the assembly code via LIBC_ASM_LINKAGE(gJmpBufManglers).
// Early startup initializes it with random bits.
[[gnu::visibility("hidden")]] extern std::array<uint64_t, JB_MANGLE_COUNT> gJmpBufManglers
    LIBC_ASM_LINKAGE_DECLARE(gJmpBufManglers);

// TODO(https://fxbug.dev/42076381): The size has been expanded to accommodate a checksum
// word, but this is not yet used until callers can be expected to use the new
// larger size.
#define JB_COUNT_UNUSED 1

static_assert(sizeof(__jmp_buf) == sizeof(uint64_t) * (JB_COUNT + JB_COUNT_UNUSED),
              "fix __jmp_buf definition");

}  // namespace LIBC_NAMESPACE_DECL

#else  // clang-format off

// This is just a shorthand to define all the variants of the names.
.macro jmp_buf.llvm_libc_function name
  .llvm_libc_function \name
  .llvm_libc_public \name
  .llvm_libc_public \name, _\name
  .llvm_libc_public \name, sig\name, weak
.endm

#endif // clang-format off

#endif  // ZIRCON_SYSTEM_ULIB_C_SETJMP_FUCHSIA_JMP_BUF_H_
