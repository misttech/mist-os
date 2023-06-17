// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_INTERNAL_LIFECYCLE_H_
#define LIB_DRIVER_COMPONENT_CPP_INTERNAL_LIFECYCLE_H_

#include "basic_factory.h"

namespace fdf_internal {

fdf::DriverStartArgs FromEncoded(const EncodedDriverStartArgs& encoded_start_args);

// |Lifecycle| implements static |Start| and |Stop| methods which will be used by the framework.
//
// By default, it will utilize |BasicFactory| to construct your primary driver class, |Driver|,
// and invoke it's |Start| method.
//
// |Driver| must inherit from |DriverBase|. If provided, |Factory| must implement a
// public |CreateDriver| function with the following signature:
// ```
// static void CreateDriver(
//     DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
// ```
//
// This illustrates how to use a |Lifecycle| with the default |BasicFactory|:
// ```
// FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(fdf::Lifecycle<MyDriver>);
// ```
//
// This illustrates how to use a |Lifecycle| with a custom factory:
// ```
// class CustomFactory {
//  public:
//   static void CreateDriver(
//       DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
//   ...construct and start driver...
// };
// // We must define the lifecycle before passing into the macro, otherwise the macro expansion
// // will think the comma is to pass a second macro argument.
// using lifecycle = fdf::Lifecycle<MyDriver, CustomFactory>;
// FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(lifecycle);
// ```
template <typename Driver, typename Factory = BasicFactory<Driver>>
class Lifecycle {
  static_assert(std::is_base_of_v<fdf::DriverBase, Driver>,
                "Driver has to inherit from DriverBase");

  DECLARE_HAS_MEMBER_FN(has_create_driver, CreateDriver);
  static_assert(has_create_driver_v<Factory>,
                "Factory must implement a public static CreateDriver function.");
  static_assert(
      std::is_same_v<decltype(&Factory::CreateDriver),
                     void (*)(fdf::DriverStartArgs, fdf::UnownedSynchronizedDispatcher,
                              fdf::StartCompleter)>,
      "CreateDriver must be a public static function with signature "
      "'void (fdf::DriverStartArgs start_args, "
      "fdf::UnownedSynchronizedDispatcher driver_dispatcher, fdf::StartCompleter completer)'.");

 public:
  static void Start(EncodedDriverStartArgs encoded_start_args, fdf_dispatcher_t* dispatcher,
                    StartCompleteCallback* complete, void* complete_cookie) {
    fdf::DriverStartArgs start_args = FromEncoded(encoded_start_args);
    fdf::StartCompleter completer(complete, complete_cookie);
    Factory::CreateDriver(std::move(start_args), fdf::UnownedSynchronizedDispatcher(dispatcher),
                          std::move(completer));
  }

  static void PrepareStop(void* driver, PrepareStopCompleteCallback* complete,
                          void* complete_cookie) {
    fdf::PrepareStopCompleter completer(complete, complete_cookie);
    static_cast<fdf::DriverBase*>(driver)->PrepareStop(std::move(completer));
  }

  static zx_status_t Stop(void* driver) {
    // Make a unique_ptr take ownership of the driver. Once it goes out of scope at the end of this
    // function, the driver is deleted automatically.
    auto driver_ptr = std::unique_ptr<fdf::DriverBase>(static_cast<fdf::DriverBase*>(driver));
    driver_ptr->Stop();
    return ZX_OK;
  }
};

#define FUCHSIA_DRIVER_LIFECYCLE_CPP_V2(lifecycle)                                \
  FUCHSIA_DRIVER_LIFECYCLE_V3(.start = lifecycle::Start, .prepare_stop = nullptr, \
                              .stop = lifecycle::Stop)

#define FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(lifecycle)                                               \
  FUCHSIA_DRIVER_LIFECYCLE_V3(.start = lifecycle::Start, .prepare_stop = lifecycle::PrepareStop, \
                              .stop = lifecycle::Stop)

}  // namespace fdf_internal

#endif  // LIB_DRIVER_COMPONENT_CPP_INTERNAL_LIFECYCLE_H_
