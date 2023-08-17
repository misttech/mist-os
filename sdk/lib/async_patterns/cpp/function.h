// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ASYNC_PATTERNS_CPP_FUNCTION_H_
#define LIB_ASYNC_PATTERNS_CPP_FUNCTION_H_

#include <lib/async_patterns/cpp/internal/task_queue.h>
#include <lib/async_patterns/cpp/sendable.h>
#include <lib/fit/function.h>
#include <zircon/assert.h>

namespace async_patterns {

template <typename Owner>
class Receiver;

// An asynchronous |Function| that will always execute on the async dispatcher
// associated with a |Receiver|. Invoking this function translates to posting a
// task to the destination dispatcher. It will not block the caller.
//
// The receiver may not necessarily receive the function call. The call will be
// a no-op if:
// - The |Receiver| object goes out of scope.
// - The async dispatcher of the |Receiver| shuts down.
//
// A function can be invoked many times, and distributed to many senders. It is
// akin to a multi-producer, single-consumer, uni-directional channel. Calls
// posted to the same |Receiver| will be processed in the order they are made,
// regardless which |Function|s and |Callback|s they are made from.
template <typename... Args>
class Function {
 public:
  // Schedules the call to be asynchronously run on the receiver's
  // dispatcher.
  //
  // See |async_patterns::BindForSending| for detailed requirements on |args|.
  void operator()(Args... args) {
    ZX_DEBUG_ASSERT(task_queue_handle_.has_value());
    task_queue_handle_.Add(
        BindForSending([f = function_](auto&&... args) { (*f)(std::move(args)...); },
                       std::forward<Args>(args)...));
  }

 private:
  // The worst case scenario is a pointer-to-member (2 words) and a weak pointer (2 words).
  // We can improve this further using custom weak pointer types if necessary.
  using FunctionType = fit::inline_function<void(Args...), sizeof(void*) * 4>;

  template <typename Owner>
  friend class ::async_patterns::Receiver;

  explicit Function(internal::TaskQueueHandle handle, FunctionType callback)
      : task_queue_handle_(std::move(handle)),
        function_(std::make_shared<FunctionType>(std::move(callback))) {}

  internal::TaskQueueHandle task_queue_handle_;

  // |function_| is reference counted because both |Function| and the remote
  // task queue need to access it. |Function| uses it to schedule more calls.
  // The task queue uses it to invoke user supplied logic.
  std::shared_ptr<FunctionType> function_;
};

}  // namespace async_patterns

#endif  // LIB_ASYNC_PATTERNS_CPP_FUNCTION_H_
