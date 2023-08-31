// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef THIRD_PARTY_PIGWEED_BACKENDS_PW_ASYNC_FUCHSIA_PUBLIC_PW_ASYNC_FUCHSIA_DISPATCHER_H_
#define THIRD_PARTY_PIGWEED_BACKENDS_PW_ASYNC_FUCHSIA_PUBLIC_PW_ASYNC_FUCHSIA_DISPATCHER_H_

#include <zircon/assert.h>

#include "pw_async/dispatcher.h"
#include "pw_async/task.h"

namespace pw::async::fuchsia {

class FuchsiaDispatcher final : public Dispatcher {
 public:
  explicit FuchsiaDispatcher(async_dispatcher_t* dispatcher) : dispatcher_(dispatcher) {}
  ~FuchsiaDispatcher() = default;

  chrono::SystemClock::time_point now() override;

  void PostAt(Task& task, chrono::SystemClock::time_point time) override;

  bool Cancel(Task& task) override;

 private:
  async_dispatcher_t* dispatcher_;
};

}  // namespace pw::async::fuchsia

#endif  // THIRD_PARTY_PIGWEED_BACKENDS_PW_ASYNC_FUCHSIA_PUBLIC_PW_ASYNC_FUCHSIA_DISPATCHER_H_
