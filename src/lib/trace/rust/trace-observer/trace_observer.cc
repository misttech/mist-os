// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/trace/observer.h>

#include <mutex>
#include <thread>

__BEGIN_CDECLS

void start_trace_observer_rust(void (*f)()) __attribute((visibility("default")));

__END_CDECLS

static void start_trace_observer_entry(fit::closure callback) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  trace::TraceObserver observer;
  observer.Start(loop.dispatcher(), std::move(callback));
  loop.Run();
}

void start_trace_observer_rust(void (*f)()) {
  std::thread thread(start_trace_observer_entry, std::move(f));
  thread.detach();
}
