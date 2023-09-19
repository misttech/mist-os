// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_TESTING_CPP_DRIVER_LIFECYCLE_H_
#define LIB_DRIVER_TESTING_CPP_DRIVER_LIFECYCLE_H_

#include <zircon/availability.h>

#if __Fuchsia_API_level__ >= 13

#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
#include <fidl/fuchsia.driver.framework/cpp/driver/wire.h>
#include <lib/driver/component/cpp/internal/driver_server.h>
#else
#include <lib/driver/component/cpp/internal/lifecycle.h>
#endif
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/symbols/symbols.h>
#include <lib/driver/testing/cpp/driver_runtime.h>

#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
// This is the exported driver registration symbol that the driver framework looks for.
// NOLINTNEXTLINE(bugprone-reserved-identifier)
extern "C" const DriverRegistration __fuchsia_driver_registration__;
#else
extern "C" const DriverLifecycle __fuchsia_driver_lifecycle__;
#endif

namespace fdf_testing {

using OpaqueDriverPtr = void*;

// The |DriverUnderTest| is a templated class so we pull out the non-template specifics into this
// base class so the implementation does not have to live in the header.
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
class DriverUnderTestBase : public fdf::WireAsyncEventHandler<fuchsia_driver_framework::Driver> {
#else
class DriverUnderTestBase {
#endif
 public:
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
  explicit DriverUnderTestBase(DriverRegistration driver_registration_symbol);
#else
  explicit DriverUnderTestBase(DriverLifecycle driver_lifecycle_symbol);
#endif
  virtual ~DriverUnderTestBase();

#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
  // fdf::WireAsyncEventHandler<fuchsia_driver_framework::Driver>
  void on_fidl_error(fidl::UnbindInfo error) override;

  // fdf::WireAsyncEventHandler<fuchsia_driver_framework::Driver>
  void handle_unknown_event(
      fidl::UnknownEventMetadata<fuchsia_driver_framework::Driver> metadata) override;
#endif

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
  template <typename Driver>
  Driver* GetDriver() {
    std::lock_guard guard(checker_);
#if __Fuchsia_API_level__ < FUCHSIA_HEAD
    ZX_ASSERT_MSG(driver_.has_value(), "Driver does not exist.");
    ZX_ASSERT_MSG(driver_.value().is_ok(), "Driver start did not succeed: %s.",
                  driver_.value().status_string());
    return static_cast<Driver*>(driver_.value().value());
#else
    if (!token_.has_value()) {
      return nullptr;
    }

    static_assert(
        std::is_same_v<decltype(&Driver::template GetInstanceFromTokenForTesting<Driver>),
                       Driver* (*)(void*)>,
        "GetDriver requires that "
        "Driver::GetInstanceFromTokenForTesting<Driver> must be a public static templated function "
        "with signature 'Driver* (void*)'");

    return Driver::template GetInstanceFromTokenForTesting<Driver>(token_.value());
#endif
  }

 private:
  fdf_dispatcher_t* driver_dispatcher_;
  async::synchronization_checker checker_;
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
  DriverRegistration driver_registration_symbol_;
  std::optional<void*> token_;
  fdf::WireClient<fuchsia_driver_framework::Driver> driver_client_ __TA_GUARDED(checker_);
  std::optional<fpromise::completer<zx::result<>>> stop_completer_ __TA_GUARDED(checker_);
#else
  DriverLifecycle driver_lifecycle_symbol_;
  std::optional<zx::result<OpaqueDriverPtr>> driver_ __TA_GUARDED(checker_);
  bool prepare_stop_completed_ __TA_GUARDED(checker_) = false;
#endif
};

// This is a RAII wrapper over a driver under test. On construction it initializes the driver server
// and on destruction it destroys the driver server.
//
// The |Driver| type given in the template is used to provide pass-through `->` and `*` operators
// to the given driver type.
//
// To use this class, ensure that the driver has been exported into the
// __fuchsia_driver_registration__ symbol using the FUCHSIA_DRIVER macros. Otherwise pass the
// DriverRegistration manually into this class.
//
// # Thread safety
//
// This class is thread-unsafe. Instances must be managed and used from a synchronized dispatcher.
// See
// https://fuchsia.dev/fuchsia-src/development/languages/c-cpp/thread-safe-async#synchronized-dispatcher
//
// If the driver dispatcher is the foreground dispatcher, the DriverUnderTest does not need to be
// wrapped in a DispatcherBound.
//
// If the driver dispatcher is a background dispatcher, the suggestion is to
// wrap this inside of an |async_patterns::TestDispatcherBound|.
//
// The driver registration's initialize and destroy hooks are executed from the context of the
// dispatcher that this object lives on, therefore the driver's initial dispatcher will be this
// same dispatcher.
template <typename Driver = void>
class DriverUnderTest : public DriverUnderTestBase {
 public:
#if __Fuchsia_API_level__ >= FUCHSIA_HEAD
  explicit DriverUnderTest(
      DriverRegistration driver_registration_symbol = __fuchsia_driver_registration__)
      : DriverUnderTestBase(driver_registration_symbol) {}
#else
  explicit DriverUnderTest(DriverLifecycle driver_lifecycle_symbol = __fuchsia_driver_lifecycle__)
      : DriverUnderTestBase(driver_lifecycle_symbol) {}
#endif

  Driver* operator->() { return static_cast<Driver*>(GetDriver<Driver>()); }
  Driver* operator*() { return static_cast<Driver*>(GetDriver<Driver>()); }
};

}  // namespace fdf_testing

#endif

#endif  // LIB_DRIVER_TESTING_CPP_DRIVER_LIFECYCLE_H_
