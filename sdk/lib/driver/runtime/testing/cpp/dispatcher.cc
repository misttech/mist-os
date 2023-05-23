// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/async/cpp/task.h>
#include <lib/driver/runtime/testing/cpp/dispatcher.h>
#include <lib/driver/runtime/testing/cpp/internal/wait_for.h>
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

zx::result<fdf::SynchronizedDispatcher>
TestDispatcherBuilder::CreateUnmanagedSynchronizedDispatcher(
    const void* driver, fdf::SynchronizedDispatcher::Options options, std::string_view name,
    fdf::Dispatcher::ShutdownHandler shutdown_handler) {
  ZX_ASSERT_MSG((options.value & FDF_DISPATCHER_OPTION_SYNCHRONIZATION_MASK) ==
                    FDF_DISPATCHER_OPTION_SYNCHRONIZED,
                "options.value=%u, needs to have FDF_DISPATCHER_OPTION_SYNCHRONIZED",
                options.value);

  // We need to create an additional shutdown context in addition to the fdf::Dispatcher
  // object, as the fdf::Dispatcher may be destructed before the shutdown handler
  // is called. This can happen if the raw pointer is released from the fdf::Dispatcher.
  auto dispatcher_shutdown_context =
      std::make_unique<fdf::Dispatcher::DispatcherShutdownContext>(std::move(shutdown_handler));
  fdf_dispatcher_t* dispatcher;
  zx_status_t status =
      fdf_testing_create_unmanaged_dispatcher(driver, options.value, name.data(), name.size(),
                                              dispatcher_shutdown_context->observer(), &dispatcher);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  [[maybe_unused]] auto released = dispatcher_shutdown_context.release();
  return zx::ok(fdf::SynchronizedDispatcher(dispatcher));
}

zx::result<fdf::UnsynchronizedDispatcher>
TestDispatcherBuilder::CreateUnmanagedUnsynchronizedDispatcher(
    const void* driver, fdf::UnsynchronizedDispatcher::Options options, std::string_view name,
    fdf::Dispatcher::ShutdownHandler shutdown_handler) {
  ZX_ASSERT_MSG((options.value & FDF_DISPATCHER_OPTION_SYNCHRONIZATION_MASK) ==
                    FDF_DISPATCHER_OPTION_UNSYNCHRONIZED,
                "options.value=%u, needs to have FDF_DISPATCHER_OPTION_UNSYNCHRONIZED",
                options.value);

  // We need to create an additional shutdown context in addition to the fdf::Dispatcher
  // object, as the fdf::Dispatcher may be destructed before the shutdown handler
  // is called. This can happen if the raw pointer is released from the fdf::Dispatcher.
  auto dispatcher_shutdown_context =
      std::make_unique<fdf::Dispatcher::DispatcherShutdownContext>(std::move(shutdown_handler));
  fdf_dispatcher_t* dispatcher;
  zx_status_t status =
      fdf_testing_create_unmanaged_dispatcher(driver, options.value, name.data(), name.size(),
                                              dispatcher_shutdown_context->observer(), &dispatcher);
  if (status != ZX_OK) {
    return zx::error(status);
  }
  [[maybe_unused]] auto released = dispatcher_shutdown_context.release();
  return zx::ok(fdf::UnsynchronizedDispatcher(dispatcher));
}

DefaultDispatcherSetting::DefaultDispatcherSetting(fdf_dispatcher_t* dispatcher) {
  zx_status_t status = fdf_testing_set_default_dispatcher(dispatcher);
  ZX_ASSERT_MSG(ZX_OK == status, "Failed to set default dispatcher setting: %s",
                zx_status_get_string(status));
}

DefaultDispatcherSetting::~DefaultDispatcherSetting() {
  zx_status_t status = fdf_testing_set_default_dispatcher(nullptr);
  ZX_ASSERT_MSG(ZX_OK == status, "Failed to remove default dispatcher setting: %s",
                zx_status_get_string(status));
}

TestSynchronizedDispatcher::TestSynchronizedDispatcher(DispatcherType type) {
  if (type == DispatcherType::Default) {
    zx::result result =
        StartDefault(fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "fdf-default");
    ZX_ASSERT_MSG(result.is_ok(), "Failed to start default dispatcher: %s", result.status_string());
    return;
  }

  if (type == DispatcherType::Managed) {
    zx::result result =
        StartManaged(fdf::SynchronizedDispatcher::Options::kAllowSyncCalls, "fdf-managed");
    ZX_ASSERT_MSG(result.is_ok(), "Failed to start managed dispatcher: %s", result.status_string());
    return;
  }

  ZX_ASSERT_MSG(false, "Unknown DispatcherType: %d", type);
}

TestSynchronizedDispatcher::~TestSynchronizedDispatcher() {
  // Stop is safe to call multiple times. It returns immediately if Stop has already happened.
  zx::result stop_result = Stop();
  ZX_ASSERT_MSG(stop_result.is_ok(), "Stop failed: %s", stop_result.status_string());
}

zx::result<> TestSynchronizedDispatcher::StartManaged(fdf::SynchronizedDispatcher::Options options,
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

zx::result<> TestSynchronizedDispatcher::StartDefault(fdf::SynchronizedDispatcher::Options options,
                                                      std::string_view dispatcher_name) {
  // Default dispatchers are created as unmanaged dispatchers so that they are safe to access from
  // the main thread.
  auto dispatcher = TestDispatcherBuilder::CreateUnmanagedSynchronizedDispatcher(
      this, options, dispatcher_name,
      [this](fdf_dispatcher_t* dispatcher) { dispatcher_shutdown_.Signal(); });
  if (dispatcher.is_error()) {
    return dispatcher.take_error();
  }
  dispatcher_ = std::move(dispatcher.value());
  default_dispatcher_setting_.emplace(dispatcher_.get());
  return zx::ok();
}

zx::result<> TestSynchronizedDispatcher::Stop() {
  dispatcher_.ShutdownAsync();
  zx::result<> stop_result = WaitFor(dispatcher_shutdown_);
  default_dispatcher_setting_.reset();
  return stop_result;
}

const TestSynchronizedDispatcher::DispatcherType kDispatcherDefault =
    TestSynchronizedDispatcher::DispatcherType::Default;

const TestSynchronizedDispatcher::DispatcherType kDispatcherManaged =
    TestSynchronizedDispatcher::DispatcherType::Managed;

}  // namespace fdf
