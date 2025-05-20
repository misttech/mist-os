// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSTEM_ULIB_C_THREADS_SHADOW_CALL_STACK_H_
#define ZIRCON_SYSTEM_ULIB_C_THREADS_SHADOW_CALL_STACK_H_

#include <type_traits>

#include "src/__support/macros/config.h"

namespace LIBC_NAMESPACE_DECL {

// Classes can use `[[no_unique_address]] IfShadowCallStack<T> member_;` along
// with `if constexpr (kShadowCallStackAbi)` guarding using `member_` as a T or
// separate overloads for NoShadowCallStack and T.

#if defined(__x86_64__)
inline constexpr bool kShadowCallStackAbi = false;
#else
inline constexpr bool kShadowCallStackAbi = true;
#endif

struct NoShadowCallStack {};

template <typename T>
using IfShadowCallStack = std::conditional_t<kShadowCallStackAbi, T, NoShadowCallStack>;

}  // namespace LIBC_NAMESPACE_DECL

#endif  // ZIRCON_SYSTEM_ULIB_C_THREADS_SHADOW_CALL_STACK_H_
