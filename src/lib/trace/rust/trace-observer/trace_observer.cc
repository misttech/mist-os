// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/trace/observer.h>
#include <zircon/threads.h>

#include <thread>

constexpr char OBSERVER_THREAD_NAME[] = "tracing:observer";
__BEGIN_CDECLS

void start_trace_observer_rust(void (*f)()) __attribute((visibility("default")));

__END_CDECLS

namespace {
void start_trace_observer_entry(fit::closure callback) {
  async::Loop loop(&kAsyncLoopConfigAttachToCurrentThread);
  trace::TraceObserver observer;
  observer.Start(loop.dispatcher(), std::move(callback));
  loop.Run();
}
}  // namespace

void start_trace_observer_rust(void (*f)()) {
  std::thread thread(start_trace_observer_entry, f);
  zx_handle_t thread_handle = native_thread_get_zx_handle(thread.native_handle());
  zx_object_set_property(thread_handle, ZX_PROP_NAME, OBSERVER_THREAD_NAME,
                         sizeof(OBSERVER_THREAD_NAME));
  thread.detach();
}
