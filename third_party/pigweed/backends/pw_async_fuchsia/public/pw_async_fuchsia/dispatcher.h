// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef THIRD_PARTY_PIGWEED_BACKENDS_PW_ASYNC_FUCHSIA_PUBLIC_PW_ASYNC_FUCHSIA_DISPATCHER_H_
#define THIRD_PARTY_PIGWEED_BACKENDS_PW_ASYNC_FUCHSIA_PUBLIC_PW_ASYNC_FUCHSIA_DISPATCHER_H_

#include <lib/async-loop/default.h>

#include "pw_async/dispatcher.h"
#include "pw_async/task.h"

namespace pw_async_fuchsia {

struct AllocatedTaskAndFunction {
  pw::async::Task task;
  pw::async::TaskFunction func;
};

// TODO(fxbug.dev/125129): Replace these temporary allocating utilities.
inline void PostAt(pw::async::Dispatcher* dispatcher, pw::async::TaskFunction&& task,
                   pw::chrono::SystemClock::time_point time) {
  AllocatedTaskAndFunction* t = new AllocatedTaskAndFunction();
  t->func = std::move(task);
  t->task.set_function([t](pw::async::Context& ctx, pw::Status status) {
    t->func(ctx, status);
    delete t;
  });
  dispatcher->PostAt(t->task, time);
}

inline void PostAfter(pw::async::Dispatcher* dispatcher, pw::async::TaskFunction&& task,
                      pw::chrono::SystemClock::duration delay) {
  PostAt(dispatcher, std::move(task), dispatcher->now() + delay);
}

inline void Post(pw::async::Dispatcher* dispatcher, pw::async::TaskFunction&& task) {
  PostAt(dispatcher, std::move(task), dispatcher->now());
}

}  // namespace pw_async_fuchsia

namespace pw::async::fuchsia {

class FuchsiaDispatcher final : public Dispatcher {
 public:
  explicit FuchsiaDispatcher() {
    async_loop_create(&kAsyncLoopConfigNoAttachToCurrentThread, &loop_);
  }
  ~FuchsiaDispatcher() override { DestroyLoop(); }

  void DestroyLoop();

  chrono::SystemClock::time_point now() override;

  void PostAt(Task& task, chrono::SystemClock::time_point time) override;
  // No-op
  void PostPeriodicAt(Task& task, chrono::SystemClock::duration interval,
                      chrono::SystemClock::time_point start_time) override;
  bool Cancel(Task& task) override;

  void RunUntilIdle();
  void RunUntil(chrono::SystemClock::time_point end_time);
  void RunFor(chrono::SystemClock::duration duration);

 private:
  async_loop_t* loop_;
};

}  // namespace pw::async::fuchsia

#endif  // THIRD_PARTY_PIGWEED_BACKENDS_PW_ASYNC_FUCHSIA_PUBLIC_PW_ASYNC_FUCHSIA_DISPATCHER_H_
