// Copyright 2019 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async-loop/cpp/loop.h>
#include <lib/async-loop/default.h>
#include <lib/trace-provider/provider.h>
#include <zircon/compiler.h>

#include <latch>
#include <mutex>
#include <thread>

__BEGIN_CDECLS

void trace_provider_create_with_fdio_rust() __attribute__((visibility("default")));
void trace_provider_wait_for_init() __attribute__((visibility("default")));

__END_CDECLS

static std::once_flag init_once;
static std::latch provider_initialized(1);

// The C++ trace provider API depends on libasync. Create a new thread here
// to run a libasync loop here to host that trace provider.
//
// This is intended to be a temporary solution until we have a proper rust
// trace-provider implementation.
//
// Shouldn't be calling this more than once during the lifetime of your program.
static void trace_provider_with_fdio_thread_entry() {
  async::Loop loop(&kAsyncLoopConfigNoAttachToCurrentThread);
  std::unique_ptr<trace::TraceProviderWithFdio> trace_provider;
  bool result = trace::TraceProviderWithFdio::CreateSynchronously(loop.dispatcher(), nullptr,
                                                                  &trace_provider, nullptr);

  if (result && trace_provider && trace_provider->is_valid()) {
    // Make sure the provider has a chance to acknowledge any already-running traces.
    loop.RunUntilIdle();

    // Tell the spawning thread that the provider has been fully initialized.
    provider_initialized.count_down();

    // Continue running the loop to handle future trace start/stop messages.
    loop.Run();
  }
}

// Calling this function multiple times is idempotent, to ensure that resources
// for the trace provider are created only once.
void trace_provider_create_with_fdio_rust() {
  std::call_once(init_once, [] {
    std::thread thread(trace_provider_with_fdio_thread_entry);
    thread.detach();
  });
}

void trace_provider_wait_for_init() { provider_initialized.wait(); }
