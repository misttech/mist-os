// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ASYNC_PATTERNS_CPP_INTERNAL_PROMISE_ADAPTOR_H_
#define LIB_ASYNC_PATTERNS_CPP_INTERNAL_PROMISE_ADAPTOR_H_

#include <lib/async_patterns/cpp/internal/task_queue.h>
#include <lib/fpromise/bridge.h>
#include <zircon/assert.h>

#include <utility>

namespace async_patterns::internal {

template <typename T>
class Tag {};

template <typename ReturnType, typename Callable>
class PromiseAdaptor {
 public:
  explicit PromiseAdaptor(Callable&& callable, internal::TaskQueueHandle handle, Tag<ReturnType>)
      : callable_(std::forward<Callable>(callable)), task_queue_handle_(std::move(handle)) {}

  PromiseAdaptor(const PromiseAdaptor&) = delete;
  PromiseAdaptor& operator=(const PromiseAdaptor&) = delete;
  PromiseAdaptor(PromiseAdaptor&&) noexcept = delete;
  PromiseAdaptor& operator=(PromiseAdaptor&&) noexcept = delete;

  // Make the call if the object is not converted to a promise.
  ~PromiseAdaptor() {
    if (task_queue_handle_.has_value()) {
      task_queue_handle_.Add(std::move(callable_));
      return;
    }
  }

  // Make the call and return a promise.
  fpromise::promise<ReturnType> promise() && {
    ZX_DEBUG_ASSERT(task_queue_handle_.has_value());
    fpromise::bridge<ReturnType> bridge;
    task_queue_handle_.Add([callable = std::move(callable_),
                            completer = bridge.completer.bind()](auto&&... args) mutable {
      using CallableReturnType = decltype(callable(std::move(args)...));
      static_assert(std::is_convertible_v<CallableReturnType, ReturnType>);
      if constexpr (std::is_void_v<ReturnType>) {
        callable(std::move(args)...);
        completer();
      } else {
        completer(callable(std::move(args)...));
      }
    });
    task_queue_handle_.reset();
    return bridge.consumer.promise();
  }

 private:
  Callable callable_;
  internal::TaskQueueHandle task_queue_handle_;
};

}  // namespace async_patterns::internal

#endif  // LIB_ASYNC_PATTERNS_CPP_INTERNAL_PROMISE_ADAPTOR_H_
