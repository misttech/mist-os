// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "sdk/lib/driver/runtime/testing/runtime/dispatcher.h"

namespace fdf {

void RunOnDispatcherSync(async_dispatcher_t* dispatcher, fit::closure task) {
  libsync::Completion task_completion;
  async::PostTask(dispatcher, [task = std::move(task), &task_completion]() {
    task();
    task_completion.Signal();
  });

  task_completion.Wait();
}

TestSynchronizedDispatcher::~TestSynchronizedDispatcher() {
  ZX_ASSERT_MSG(!dispatcher_.has_value(),
                "TestSynchronizedDispatcher must have Stop called before shutting down");
}

zx::result<> TestSynchronizedDispatcher::Start(std::string_view dispatcher_name) {
  if (dispatcher_.has_value()) {
    return zx::error(ZX_ERR_ALREADY_EXISTS);
  }

  fdf_testing_push_driver(this);
  zx::result dispatcher = fdf::SynchronizedDispatcher::Create(
      fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, dispatcher_name,
      [this](fdf_dispatcher_t* dispatcher) { dispatcher_shutdown_.Signal(); });
  if (dispatcher.is_error()) {
    fdf_testing_pop_driver();
    return dispatcher.take_error();
  }
  dispatcher_ = std::move(dispatcher.value());

  fdf_testing_pop_driver();
  return zx::ok();
}

zx::result<> TestSynchronizedDispatcher::Stop() {
  if (!dispatcher_.has_value()) {
    return zx::ok();
  }

  dispatcher_.value().ShutdownAsync();
  if (zx_status_t status = dispatcher_shutdown_.Wait(); status != ZX_OK) {
    return zx::error(status);
  }
  dispatcher_.reset();
  dispatcher_shutdown_.Reset();
  return zx::ok();
}

}  // namespace fdf
