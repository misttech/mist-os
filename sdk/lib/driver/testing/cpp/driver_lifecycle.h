// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_TESTING_CPP_DRIVER_LIFECYCLE_H_
#define LIB_DRIVER_TESTING_CPP_DRIVER_LIFECYCLE_H_

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/internal/lifecycle.h>
#include <lib/driver/runtime/testing/cpp/dispatcher.h>
#include <lib/driver/symbols/symbols.h>
#include <lib/driver/testing/cpp/driver_runtime.h>

// This is the exported driver lifecycle symbol that the driver framework looks for.
// NOLINTNEXTLINE(bugprone-reserved-identifier)
extern "C" const DriverLifecycle __fuchsia_driver_lifecycle__;

namespace fdf_testing {

using OpaqueDriverPtr = void*;

// The |DriverUnderTest| is a templated class so we pull out the non-template specifics into this
// base class so the implementation does not have to live in the header.
class DriverUnderTestBase {
 public:
  explicit DriverUnderTestBase(DriverLifecycle driver_lifecycle_symbol);
  virtual ~DriverUnderTestBase();

  // Start the driver. This is an asynchronous operation.
  // Use |DriverRuntime::RunToCompletion| to await the completion of the async task.
  // The resulting zx::result is the result of the start operation.
  DriverRuntime::AsyncTask<zx::result<>> Start(fdf::DriverStartArgs start_args);

  // PrepareStop the driver. This is an asynchronous operation.
  // Use |DriverRuntime::RunToCompletion| to await the completion of the async task.
  // The resulting zx::result is the result of the prepare stop operation.
  DriverRuntime::AsyncTask<zx::result<>> PrepareStop();

  // Stop the driver. The PrepareStop operation must have been completed before Stop is called.
  // Returns the result of the stop operation.
  zx::result<> Stop();

 protected:
  void* GetDriver();

 private:
  fdf_dispatcher_t* driver_dispatcher_;
  DriverLifecycle driver_lifecycle_symbol_;
  async::synchronization_checker checker_;
  std::optional<zx::result<OpaqueDriverPtr>> driver_ __TA_GUARDED(checker_);
  std::optional<std::promise<zx::result<>>> start_promise_ __TA_GUARDED(checker_);
  std::optional<std::promise<zx::result<>>> prepare_stop_promise_ __TA_GUARDED(checker_);
  std::shared_future<zx::result<>> prepare_stop_promise_future_ __TA_GUARDED(checker_);
};

// This is a RAII wrapper over a driver under test. On destruction, it will call |Stop| for the
// driver if it hasn't already been called, but |PrepareStop| must have been manually called and
// awaited.
//
// The |Driver| type given in the template is used to provide pass-through `->` and `*` operators
// to the given driver type.
//
// To use this class, ensure that the driver has been exported into the
// __fuchsia_driver_lifecycle__ symbol using the FUCHSIA_DRIVER macros. Otherwise pass the
// DriverLifecycle manually into this class.
//
// # Thread safety
//
// This class is thread-unsafe. Instances must be managed and used from a synchronized dispatcher.
// See
// https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/thread-safe-async#synchronized-dispatcher
//
// If the driver dispatcher is the default dispatcher, the DriverUnderTest does not need to be
// wrapped in a DispatcherBound. Example:
// ```
// fdf::TestSynchronizedDispatcher test_dispatcher_{fdf::kDispatcherDefault};
// fdf_testing::DriverUnderTest driver_;
// ```
//
// If the driver dispatcher is not the default dispatcher, the suggestion is to
// wrap this inside of an |async_patterns::TestDispatcherBound|. Example:
// ```
// fdf::TestSynchronizedDispatcher test_dispatcher_{fdf::kDispatcherManaged};
// async_patterns::TestDispatcherBound<fdf_testing::TestSynchronizedDispatcher> driver_{
//      test_dispatcher_.dispatcher(), std::in_place};
// ```
template <typename Driver = void>
class DriverUnderTest : public DriverUnderTestBase {
 public:
  explicit DriverUnderTest(DriverLifecycle driver_lifecycle_symbol = __fuchsia_driver_lifecycle__)
      : DriverUnderTestBase(driver_lifecycle_symbol) {}

  Driver* operator->() { return static_cast<Driver*>(GetDriver()); }
  Driver* operator*() { return static_cast<Driver*>(GetDriver()); }
};

}  // namespace fdf_testing

#endif  // LIB_DRIVER_TESTING_CPP_DRIVER_LIFECYCLE_H_
