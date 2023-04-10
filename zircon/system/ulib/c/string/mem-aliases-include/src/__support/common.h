// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC___SUPPORT_COMMON_H_
#define SRC___SUPPORT_COMMON_H_

#include_next "src/__support/common.h"

#undef LLVM_LIBC_FUNCTION_IMPL

#define LLVM_LIBC_FUNCTION_IMPL(type, name, arglist)                                      \
  LLVM_LIBC_FUNCTION_ATTR decltype(__llvm_libc::name) __##name##_impl__ __asm__(#name);   \
  decltype(__llvm_libc::name) name [[gnu::alias(#name)]];                                 \
  extern "C" decltype(__llvm_libc::name) __libc_##name [[gnu::alias(#name)]];             \
  IF_LIBC_UNSANITIZED(                                                                    \
      extern "C" LLVM_LIBC_FUNCTION_ATTR decltype(__llvm_libc::name) __unsanitized_##name \
      [[gnu::alias(#name)]];)                                                             \
  type __##name##_impl__ arglist

#ifdef LIBC_UNSANITIZED
#define IF_LIBC_UNSANITIZED(x) x
#else
#define IF_LIBC_UNSANITIZED(x)
#endif

#endif  // SRC___SUPPORT_COMMON_H_
