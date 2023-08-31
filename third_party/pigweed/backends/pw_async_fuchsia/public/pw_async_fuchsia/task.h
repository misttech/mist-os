// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef THIRD_PARTY_PIGWEED_BACKENDS_PW_ASYNC_FUCHSIA_PUBLIC_PW_ASYNC_FUCHSIA_TASK_H_
#define THIRD_PARTY_PIGWEED_BACKENDS_PW_ASYNC_FUCHSIA_PUBLIC_PW_ASYNC_FUCHSIA_TASK_H_

#include <lib/async/task.h>

#include "pw_async/task_function.h"
#include "pw_chrono/system_clock.h"

namespace pw::async::fuchsia {

// NativeTask friend forward declaration.
class FuchsiaDispatcher;

}  // namespace pw::async::fuchsia

namespace pw::async::test::backend {

// NativeTask friend forward declaration.
class NativeFakeDispatcher;

}  // namespace pw::async::test::backend

namespace pw::async::backend {

class NativeTask final : public async_task_t {
 private:
  friend class ::pw::async::fuchsia::FuchsiaDispatcher;
  friend class ::pw::async::test::backend::NativeFakeDispatcher;
  friend class ::pw::async::Task;

  explicit NativeTask(::pw::async::Task& task);
  explicit NativeTask(::pw::async::Task& task, TaskFunction&& f);

  void operator()(Context& ctx, Status status);

  void set_function(TaskFunction&& f);

  chrono::SystemClock::time_point due_time() const;

  void set_due_time(chrono::SystemClock::time_point due_time);

  static void Handler(async_dispatcher_t* /*dispatcher*/, async_task_t* task, zx_status_t status);

  TaskFunction func_;
  Task& task_;
  // `dispatcher_` is set by a Dispatcher to its own address before forwarding a task to the
  // underlying Zircon async-loop, so that the Dispatcher pointer in the Context may be set in a
  // type-safe manner inside `Handler` when invoked by the async-loop.
  Dispatcher* dispatcher_ = nullptr;
};

using NativeTaskHandle = NativeTask&;

}  // namespace pw::async::backend

#endif  // THIRD_PARTY_PIGWEED_BACKENDS_PW_ASYNC_FUCHSIA_PUBLIC_PW_ASYNC_FUCHSIA_TASK_H_
