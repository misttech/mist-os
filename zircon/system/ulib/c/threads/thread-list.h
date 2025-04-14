// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef ZIRCON_SYSTEM_ULIB_C_THREADS_THREAD_LIST_H_
#define ZIRCON_SYSTEM_ULIB_C_THREADS_THREAD_LIST_H_

#include <zircon/compiler.h>

#include <concepts>
#include <functional>
#include <iterator>
#include <ranges>
#include <type_traits>

#include "../asm-linkage.h"
#include "mutex.h"
#include "src/__support/macros/config.h"
#include "threads_impl.h"

namespace LIBC_NAMESPACE_DECL {

using Thread = ::__pthread;

// TODO(https://fxbug.dev/342469121): asm-linkage only needed for basic_abi
// musl glue.
extern Mutex gAllThreadsLock LIBC_ASM_LINKAGE_DECLARE(gAllThreadsLock) __LOCAL;
extern Thread* gAllThreads LIBC_ASM_LINKAGE_DECLARE(gAllThreads) __LOCAL
    __TA_GUARDED(gAllThreadsLock);

// This just wraps T so that its operator++ calls Increment as T(T).
// The * and -> operators are passed through for a pointer type.
// Instantiations satisfy std::incrementable.
template <typename T, std::regular_invocable<T> auto Increment>
  requires std::convertible_to<std::invoke_result_t<decltype(Increment), T>, T>
struct Incrementable {
  using difference_type = std::incrementable_traits<T>::difference_type;

  constexpr bool operator==(const Incrementable&) const = default;
  constexpr auto operator<=>(const Incrementable&) const = default;

  constexpr Incrementable& operator++() {  // prefix
    value = std::invoke(Increment, value);
    return *this;
  }

  constexpr Incrementable operator++(int) {  // postfix
    Incrementable result = *this;
    value = std::invoke(Increment, value);
    return result;
  }

  template <typename = void>
    requires(std::is_pointer_v<T>)
  constexpr auto* operator->() const {
    return value;
  }

  template <typename = void>
    requires(std::is_pointer_v<T>)
  constexpr auto* operator->() {
    return value;
  }

  template <typename = void>
    requires(std::is_pointer_v<T>)
  constexpr auto& operator*() const {
    return *value;
  }

  template <typename = void>
    requires(std::is_pointer_v<T>)
  constexpr auto& operator*() {
    return *value;
  }

  T value{};
};

using IncrementableThread = Incrementable<Thread*, &Thread::next>;
static_assert(std::weakly_incrementable<IncrementableThread>);

using ThreadList = std::ranges::iota_view<IncrementableThread, IncrementableThread>;

__TA_REQUIRES(gAllThreadsLock) inline ThreadList AllThreadsLocked() {
  return ThreadList{IncrementableThread{gAllThreads}};
}

class __TA_SCOPED_CAPABILITY AllThreads : public ThreadList {
 public:
  AllThreads() __TA_ACQUIRE(gAllThreadsLock) : ThreadList{IncrementableThread{gAllThreads}} {}

  ~AllThreads() __TA_RELEASE() = default;

  Thread* find(Thread* tcb) const {
    auto it = std::ranges::find(*this, IncrementableThread{tcb});
    return it == end() ? nullptr : (*it).value;
  }

  Thread* FindTp(uintptr_t tp) const { return FindTp(reinterpret_cast<void*>(tp)); }

  Thread* FindTp(void* tp) const {
    // In a race with a freshly-created thread setting up its thread
    // pointer, it might still be zero.
    return tp ? find(tp_to_pthread(tp)) : nullptr;
  }

 private:
  std::lock_guard<Mutex> lock_{gAllThreadsLock};
};

}  // namespace LIBC_NAMESPACE_DECL

#endif  // ZIRCON_SYSTEM_ULIB_C_THREADS_THREAD_LIST_H_
