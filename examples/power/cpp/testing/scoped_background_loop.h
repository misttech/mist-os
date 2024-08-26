// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef EXAMPLES_POWER_CPP_TESTING_SCOPED_BACKGROUND_LOOP_H_
#define EXAMPLES_POWER_CPP_TESTING_SCOPED_BACKGROUND_LOOP_H_

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/async/dispatcher.h>

namespace examples::power::testing {

// Runs an async loop on a background thread and cleans up when this object exits scope.
class ScopedBackgroundLoop {
 public:
  explicit ScopedBackgroundLoop() : loop_(&kAsyncLoopConfigNoAttachToCurrentThread) {
    loop_.StartThread();
  }
  ~ScopedBackgroundLoop() {
    loop_.Shutdown();
    loop_.JoinThreads();
  }

  async_dispatcher_t* dispatcher() const { return loop_.dispatcher(); }

 private:
  async::Loop loop_;
};

}  // namespace examples::power::testing

#endif  // EXAMPLES_POWER_CPP_TESTING_SCOPED_BACKGROUND_LOOP_H_
