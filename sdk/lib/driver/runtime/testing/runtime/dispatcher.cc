// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/driver/runtime/testing/runtime/dispatcher.h"

#include <lib/async/cpp/task.h>
#include <lib/fdf/cpp/env.h>
#include <lib/fdf/testing.h>

namespace fdf {

void RunOnDispatcherSync(async_dispatcher_t* dispatcher, fit::closure task) {
  libsync::Completion task_completion;
  async::PostTask(dispatcher, [task = std::move(task), &task_completion]() {
    task();
    task_completion.Signal();
  });

  task_completion.Wait();
}

zx::result<> TestSynchronizedDispatcher::Start(fdf::SynchronizedDispatcher::Options options,
                                               std::string_view dispatcher_name) {
  auto dispatcher = fdf_env::DispatcherBuilder::CreateSynchronizedWithOwner(
      this, options, dispatcher_name,
      [this](fdf_dispatcher_t* dispatcher) { dispatcher_shutdown_.Signal(); });
  if (dispatcher.is_error()) {
    return dispatcher.take_error();
  }
  dispatcher_ = std::move(dispatcher.value());
  return zx::ok();
}

zx::result<> TestSynchronizedDispatcher::StartAsDefault(
    fdf::SynchronizedDispatcher::Options options, std::string_view dispatcher_name) {
  bool allow_sync_calls = options.value & FDF_DISPATCHER_OPTION_ALLOW_SYNC_CALLS;
  if (allow_sync_calls) {
    return zx::error(ZX_ERR_INVALID_ARGS);
  }

  if (zx::result result = Start(options, dispatcher_name); result.is_error()) {
    return result.take_error();
  }

  return zx::make_result(fdf_testing_set_default_dispatcher(dispatcher_.get()));
}

zx::result<> TestSynchronizedDispatcher::StopSync() {
  StopAsync();
  return WaitForStop();
}

void TestSynchronizedDispatcher::StopAsync() { dispatcher_.ShutdownAsync(); }

zx::result<> TestSynchronizedDispatcher::WaitForStop() {
  if (zx_status_t status = dispatcher_shutdown_.Wait(); status != ZX_OK) {
    return zx::error(status);
  }
  return zx::ok();
}

}  // namespace fdf
