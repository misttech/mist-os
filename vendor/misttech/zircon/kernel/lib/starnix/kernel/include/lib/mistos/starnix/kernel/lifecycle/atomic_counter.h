// Copyright 2024 Mist Tecnologia LTDA. All rights reserved.
// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

//! Helper class to implement a counter that can be shared across threads.

#ifndef ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_LIFECYCLE_ATOMIC_COUNTER_H_
#define ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_LIFECYCLE_ATOMIC_COUNTER_H_

#include <ktl/atomic.h>

namespace starnix {

template <typename T>
class AtomicCounter {
 public:
  static constexpr AtomicCounter New(T value) { return AtomicCounter(value); }

  T next() const { return add(static_cast<T>(1)); }

  T add(T amount) const { return value_.fetch_add(amount, std::memory_order_relaxed); }

  T get() const { return value_.load(std::memory_order_relaxed); }

  void reset(T value) { value_.store(value, std::memory_order_relaxed); }

 public:
  AtomicCounter() = default;

 private:
  explicit AtomicCounter(T value) : value_(value) {}

  mutable ktl::atomic<T> value_ = 0;
};

}  // namespace starnix

#endif  // ZIRCON_KERNEL_LIB_MISTOS_STARNIX_KERNEL_INCLUDE_LIB_MISTOS_STARNIX_KERNEL_LIFECYCLE_ATOMIC_COUNTER_H_
