// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pw_async_fuchsia/task.h"

#include <zircon/assert.h>

#include "pw_async_fuchsia/util.h"

namespace pw::async::backend {

NativeTask::NativeTask(::pw::async::Task& task)
    : async_task_t{{ASYNC_STATE_INIT}, &NativeTask::Handler, {}}, task_(task) {}
NativeTask::NativeTask(::pw::async::Task& task, TaskFunction&& f)
    : async_task_t{{ASYNC_STATE_INIT}, &NativeTask::Handler, {}},
      func_(std::move(f)),
      task_(task) {}

void NativeTask::operator()(Context& ctx, Status status) { func_(ctx, status); }

void NativeTask::set_function(TaskFunction&& f) { func_ = std::move(f); }

pw::chrono::SystemClock::time_point NativeTask::due_time() const {
  return pw_async_fuchsia::ZxTimeToTimepoint(zx::time{deadline});
}

void NativeTask::set_due_time(chrono::SystemClock::time_point due_time) {
  deadline = pw_async_fuchsia::TimepointToZxTime(due_time).get();
}

void NativeTask::Handler(async_dispatcher_t* /*dispatcher*/, async_task_t* task,
                         zx_status_t status) {
  auto self = static_cast<NativeTask*>(task);
  Context c{.dispatcher = self->dispatcher_, .task = &self->task_};
  (*self)(c, status == ZX_OK ? OkStatus() : Status::Cancelled());
}

}  // namespace pw::async::backend
