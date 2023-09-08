// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC___SUPPORT_COMMON_H_
#define SRC___SUPPORT_COMMON_H_

#include_next "src/__support/common.h"

#undef LLVM_LIBC_FUNCTION_IMPL

#define LLVM_LIBC_FUNCTION_IMPL(type, name, arglist)                                         \
  LLVM_LIBC_FUNCTION_ATTR decltype(LIBC_NAMESPACE::name) __##name##_impl__ __asm__(#name);   \
  decltype(LIBC_NAMESPACE::name) name [[gnu::alias(#name)]];                                 \
  MEM_INTERNAL_NAME(name, __libc_)                                                           \
  MEM_INTERNAL_NAME(name, __asan_, gnu::weak)                                                \
  MEM_INTERNAL_NAME(name, __hwasan_, gnu::weak)                                              \
  IF_LIBC_UNSANITIZED(                                                                       \
      extern "C" LLVM_LIBC_FUNCTION_ATTR decltype(LIBC_NAMESPACE::name) __unsanitized_##name \
      [[gnu::alias(#name)]];)                                                                \
  type __##name##_impl__ arglist

// Since this code is built without sanitizers, it unconditionally defines the
// (hidden) weak __asan_ and __hwasan_ symbols that are needed for the
// sanitized libc.so not to have any undefined symbols.  These are never
// exported so it doesn't matter that they're defined in unsanitized builds.

#define MEM_INTERNAL_NAME(name, prefix, ...) \
  extern "C" decltype(LIBC_NAMESPACE::name) prefix##name [[gnu::alias(#name), ##__VA_ARGS__]];

#ifdef LIBC_UNSANITIZED
#define IF_LIBC_UNSANITIZED(x) x
#else
#define IF_LIBC_UNSANITIZED(x)
#endif

#endif  // SRC___SUPPORT_COMMON_H_
