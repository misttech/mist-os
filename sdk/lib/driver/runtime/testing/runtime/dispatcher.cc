// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/driver/runtime/testing/runtime/dispatcher.h"

#include <lib/async/cpp/task.h>
#include <lib/driver/runtime/testing/runtime/internal/wait_for.h>
#include <lib/fdf/cpp/env.h>
#include <lib/fdf/testing.h>

namespace fdf {

zx::result<> RunOnDispatcherSync(async_dispatcher_t* dispatcher, fit::closure task) {
  libsync::Completion task_completion;
  async::PostTask(dispatcher, [task = std::move(task), &task_completion]() {
    task();
    task_completion.Signal();
  });

  return WaitFor(task_completion);
}

zx::result<> WaitFor(libsync::Completion& completion) {
  zx::result wait_result =
      internal::CheckManagedThreadOrWaitUntil([&] { return completion.signaled(); });
  if (wait_result.is_error()) {
    return wait_result.take_error();
  }

  completion.Wait();
  return zx::ok();
}

TestSynchronizedDispatcher::TestSynchronizedDispatcher(const DispatcherStartArgs& args) {
  if (args.is_default_dispatcher) {
    zx::result result = StartAsDefault(args.options, args.dispatcher_name);
    ZX_ASSERT_MSG(result.is_ok(), "Failed to start dispatcher '%s' as default: %s",
                  args.dispatcher_name.c_str(), result.status_string());
  } else {
    zx::result result = Start(args.options, args.dispatcher_name);
    ZX_ASSERT_MSG(result.is_ok(), "Failed to start dispatcher '%s': %s",
                  args.dispatcher_name.c_str(), result.status_string());
  }
}

TestSynchronizedDispatcher::~TestSynchronizedDispatcher() {
  // Stop is safe to call multiple times. It returns immediately if Stop has already happened.
  zx::result stop_result = Stop();
  ZX_ASSERT_MSG(stop_result.is_ok(), "Stop failed: %s", stop_result.status_string());
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

zx::result<> TestSynchronizedDispatcher::Stop() {
  dispatcher_.ShutdownAsync();
  return WaitFor(dispatcher_shutdown_);
}

const TestSynchronizedDispatcher::DispatcherStartArgs kDispatcherDefault = {
    .is_default_dispatcher = true,
    .options = {},
    .dispatcher_name = "test-fdf-dispatcher-default",
};

const TestSynchronizedDispatcher::DispatcherStartArgs kDispatcherNoDefault = {
    .is_default_dispatcher = false,
    .options = {},
    .dispatcher_name = "test-fdf-dispatcher",
};

const TestSynchronizedDispatcher::DispatcherStartArgs kDispatcherNoDefaultAllowSync = {
    .is_default_dispatcher = false,
    .options = fdf::SynchronizedDispatcher::Options::kAllowSyncCalls,
    .dispatcher_name = "test-fdf-dispatcher-allow-sync",
};

}  // namespace fdf
