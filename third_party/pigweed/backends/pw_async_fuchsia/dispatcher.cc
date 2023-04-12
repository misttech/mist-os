// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pw_async_fuchsia/dispatcher.h"

#include <lib/async/cpp/time.h>
#include <zircon/assert.h>

#include "pw_async_fuchsia/util.h"

namespace pw::async::fuchsia {

void FuchsiaDispatcher::DestroyLoop() {
  if (loop_) {
    async_loop_t* loop = loop_;
    loop_ = nullptr;
    // Invokes all Task Handlers with a ZX_ERR_CANCELED status.
    async_loop_destroy(loop);
  }
}

chrono::SystemClock::time_point FuchsiaDispatcher::now() {
  return pw_async_fuchsia::ZxTimeToTimepoint(zx::time{async_now(async_loop_get_dispatcher(loop_))});
}

void FuchsiaDispatcher::PostAt(Task& task, chrono::SystemClock::time_point time) {
  // TODO(fxbug.dev/125112): Return errors once these methods return a Status.
  if (!loop_) {
    Context ctx{.dispatcher = this, .task = &task};
    task(ctx, Status::Cancelled());
    return;
  }
  backend::NativeTask& native_task = task.native_type();
  native_task.set_due_time(time);
  native_task.dispatcher_ = this;
  async_post_task(async_loop_get_dispatcher(loop_), &native_task);
}

bool FuchsiaDispatcher::Cancel(Task& task) {
  if (!loop_) {
    return false;
  }
  return async_cancel_task(async_loop_get_dispatcher(loop_), &task.native_type()) == ZX_OK;
}

void FuchsiaDispatcher::RunUntilIdle() { async_loop_run_until_idle(loop_); }

void FuchsiaDispatcher::RunUntil(chrono::SystemClock::time_point end_time) {
  async_loop_run(loop_, pw_async_fuchsia::TimepointToZxTime(end_time).get(), false);
}

void FuchsiaDispatcher::RunFor(chrono::SystemClock::duration duration) {
  RunUntil(now() + duration);
}

// NO-OP
void FuchsiaDispatcher::PostPeriodicAt(Task& task, chrono::SystemClock::duration interval,
                                       chrono::SystemClock::time_point start_time) {
  // TODO(fxbug.dev/125311): Implement periodic tasks.
  ZX_PANIC("Not implemented");
  static_cast<void>(task);
  static_cast<void>(interval);
  static_cast<void>(start_time);
}

}  // namespace pw::async::fuchsia
