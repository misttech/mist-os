// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "pw_async_fuchsia/dispatcher.h"

#include <lib/async/cpp/time.h>

#include "pw_async_fuchsia/util.h"

namespace pw::async::fuchsia {

chrono::SystemClock::time_point FuchsiaDispatcher::now() {
  return pw_async_fuchsia::ZxTimeToTimepoint(zx::time{async_now(dispatcher_)});
}

void FuchsiaDispatcher::PostAt(Task& task, chrono::SystemClock::time_point time) {
  backend::NativeTask& native_task = task.native_type();
  native_task.set_due_time(time);
  native_task.dispatcher_ = this;
  async_post_task(dispatcher_, &native_task);
}

bool FuchsiaDispatcher::Cancel(Task& task) {
  return async_cancel_task(dispatcher_, &task.native_type()) == ZX_OK;
}

}  // namespace pw::async::fuchsia
