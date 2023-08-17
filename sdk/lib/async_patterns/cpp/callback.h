// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_ASYNC_PATTERNS_CPP_CALLBACK_H_
#define LIB_ASYNC_PATTERNS_CPP_CALLBACK_H_

#include <lib/async_patterns/cpp/internal/task_queue.h>
#include <lib/async_patterns/cpp/sendable.h>
#include <lib/fit/function.h>
#include <zircon/assert.h>

namespace async_patterns {

template <typename Owner>
class Receiver;

// An asynchronous |Callback| that will always execute on the async dispatcher
// associated with a |Receiver|. Invoking this callback translates to posting a
// task to the destination dispatcher. It will not block the caller.
//
// The receiver may not necessarily receive the callback. The callback will be
// a no-op if:
// - The |Receiver| object goes out of scope.
// - The async dispatcher of the |Receiver| shuts down.
//
// A callback can only be invoked once. It is akin to a one-shot,
// uni-directional channel. Calls posted to the same |Receiver| will be
// processed in the order they are made, regardless which |Function|s and
// |Callback|s they are made from.
template <typename... Args>
class Callback {
 public:
  // Schedules the callback to be asynchronously run on the receiver's
  // dispatcher.
  //
  // See |async_patterns::BindForSending| for detailed requirements on |args|.
  void operator()(Args... args) {
    ZX_DEBUG_ASSERT(task_queue_handle_.has_value());
    task_queue_handle_.Add(BindForSending(std::move(callback_), std::forward<Args>(args)...));
    task_queue_handle_.reset();
  }

 private:
  // The worst case scenario is a pointer-to-member (2 words) and a weak pointer (2 words).
  // We can improve this further using custom weak pointer types if necessary.
  using CallbackType = fit::inline_callback<void(Args...), sizeof(void*) * 4>;

  template <typename Owner>
  friend class ::async_patterns::Receiver;

  explicit Callback(internal::TaskQueueHandle handle, CallbackType callback)
      : task_queue_handle_(std::move(handle)), callback_(std::move(callback)) {}

  internal::TaskQueueHandle task_queue_handle_;
  CallbackType callback_;
};

}  // namespace async_patterns

#endif  // LIB_ASYNC_PATTERNS_CPP_CALLBACK_H_
