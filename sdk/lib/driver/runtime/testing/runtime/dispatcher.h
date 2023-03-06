// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_RUNTIME_TESTING_RUNTIME_DISPATCHER_H_
#define LIB_DRIVER_RUNTIME_TESTING_RUNTIME_DISPATCHER_H_

#include <lib/driver/runtime/testing/runtime/internal/wait_for.h>
#include <lib/fdf/cpp/dispatcher.h>
#include <lib/sync/cpp/completion.h>

#include <future>

namespace fdf {

// Run `task` on `dispatcher`, and wait until it is completed.
//
// This MUST be called from the main test thread.
zx::result<> RunOnDispatcherSync(async_dispatcher_t* dispatcher, fit::closure task);

// Wait until the completion is signaled. When this function returns, the completion is signaled.
//
// This MUST be called from the main test thread.
zx::result<> WaitFor(libsync::Completion& completion);

// Wait until the future is resolved, then returns the resolved value of the future.
//
// This MUST be called from the main test thread.
template <typename T>
zx::result<T> WaitFor(std::future<T> future) {
  using namespace std::chrono_literals;
  zx::result wait_result = internal::CheckManagedThreadOrWaitUntil(
      [&] { return future.wait_for(1ms) != std::future_status::timeout; });
  if (wait_result.is_error()) {
    return wait_result.take_error();
  }

  return zx::ok(future.get());
}

// A wrapper around an fdf::SynchronizedDispatcher that is meant for testing.
class TestSynchronizedDispatcher {
 public:
  // Arguments for starting the underlying fdf::SynchronizedDispatcher.
  struct DispatcherStartArgs {
    bool is_default_dispatcher;
    fdf::SynchronizedDispatcher::Options options;
    std::string dispatcher_name;
  };
  // This does not start the dispatcher. The |Start| or |StartAsDefault| must be called before
  // there is an underlying dispatcher to access.
  TestSynchronizedDispatcher() = default;

  // This will start the underlying dispatcher with the given parameters. If the underlying
  // dispatcher fails to start, this constructor will throw an assert.
  explicit TestSynchronizedDispatcher(const DispatcherStartArgs& args);

  // If |Stop| hasn't been called, it will get called here.
  ~TestSynchronizedDispatcher();

  // Start the dispatcher. Once this returns successfully the dispatcher is available to be
  // used for queueing and running tasks.
  //
  // This MUST be called from the main test thread.
  zx::result<> Start(fdf::SynchronizedDispatcher::Options options,
                     std::string_view dispatcher_name);

  // Start the dispatcher, and set it as the default dispatcher for the driver runtime.
  // Once this returns successfully the dispatcher is available to be used for queueing and running
  // tasks.
  // Default dispatchers are not supported alongside driver runtime managed threads.
  // FDF_DISPATCHER_OPTION_ALLOW_SYNC_CALLS causes the driver runtime to spin up managed threads,
  // therefore default dispatchers are not supported alongside that option.
  //
  // This MUST be called from the main test thread.
  //
  // Returns ZX_ERR_INVALID_ARGS if options contains FDF_DISPATCHER_OPTION_ALLOW_SYNC_CALLS.
  // Returns ZX_ERR_BAD_STATE if the driver runtime is managing any threads, which will happen if
  // there are any existing dispatchers with the FDF_DISPATCHER_OPTION_ALLOW_SYNC_CALLS option.
  zx::result<> StartAsDefault(fdf::SynchronizedDispatcher::Options options,
                              std::string_view dispatcher_name);

  // This will stop the dispatcher and wait until it stops.
  // When this function returns, the dispatcher is stopped.
  // Safe to call multiple times. It will return immediately if Stop has already happened
  //
  // This MUST be called from the main test thread.
  zx::result<> Stop();

  const fdf::SynchronizedDispatcher& driver_dispatcher() { return dispatcher_; }
  async_dispatcher_t* dispatcher() { return dispatcher_.async_dispatcher(); }

 private:
  fdf::SynchronizedDispatcher dispatcher_;
  libsync::Completion dispatcher_shutdown_;
};

extern const TestSynchronizedDispatcher::DispatcherStartArgs kDispatcherDefault;
extern const TestSynchronizedDispatcher::DispatcherStartArgs kDispatcherNoDefault;
extern const TestSynchronizedDispatcher::DispatcherStartArgs kDispatcherNoDefaultAllowSync;

}  // namespace fdf

#endif  // LIB_DRIVER_RUNTIME_TESTING_RUNTIME_DISPATCHER_H_
