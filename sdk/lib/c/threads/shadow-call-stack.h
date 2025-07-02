// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_C_THREADS_SHADOW_CALL_STACK_H_
#define LIB_C_THREADS_SHADOW_CALL_STACK_H_

#include <concepts>
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

constexpr void OnShadowCallStack(NoShadowCallStack, auto&& f) {}
constexpr void OnShadowCallStack(auto&& stack, auto&& f) {
  std::forward<decltype(f)>(f)(std::forward<decltype(stack)>(stack));
}

constexpr auto ShadowCallStackOr(NoShadowCallStack, auto value) { return value; }
constexpr auto ShadowCallStackOr(auto stack, std::convertible_to<decltype(stack)> auto value) {
  return stack;
}

}  // namespace LIBC_NAMESPACE_DECL

#endif  // LIB_C_THREADS_SHADOW_CALL_STACK_H_
