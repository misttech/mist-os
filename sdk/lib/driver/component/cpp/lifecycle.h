// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef LIB_DRIVER_COMPONENT_CPP_LIFECYCLE_H_
#define LIB_DRIVER_COMPONENT_CPP_LIFECYCLE_H_

#include <lib/driver/component/cpp/basic_factory.h>

namespace fdf {

// |Lifecycle| implements static |Start| and |Stop| methods which will be used by the framework.
//
// By default, it will utilize |BasicFactory| to construct your primary driver class, |Driver|,
// and invoke it's |Start| method.
//
// |Driver| must inherit from |DriverBase|. If provided, |Factory| must implement a
// public |CreateDriver| function with the following signature:
// ```
// static zx::result<std::unique_ptr<DriverBase>> CreateDriver(
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
//   static zx::result<std::unique_ptr<DriverBase>> CreateDriver(
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
  static_assert(std::is_base_of_v<DriverBase, Driver>, "Driver has to inherit from DriverBase");

  DECLARE_HAS_MEMBER_FN(has_create_driver, CreateDriver);
  static_assert(has_create_driver_v<Factory>,
                "Factory must implement a public static CreateDriver function.");
  static_assert(std::is_same_v<decltype(&Factory::CreateDriver),
                               zx::result<std::unique_ptr<DriverBase>> (*)(
                                   DriverStartArgs start_args,
                                   fdf::UnownedSynchronizedDispatcher driver_dispatcher)>,
                "CreateDriver must be a public static function with signature "
                "'zx::result<std::unique_ptr<fdf::DriverBase>> (fdf::DriverStartArgs start_args, "
                "fdf::UnownedSynchronizedDispatcher driver_dispatcher)'.");

 public:
  static zx_status_t Start(EncodedDriverStartArgs encoded_start_args, fdf_dispatcher_t* dispatcher,
                           void** driver) {
    // Decode the incoming `msg`.
    auto wire_format_metadata =
        fidl::WireFormatMetadata::FromOpaque(encoded_start_args.wire_format_metadata);
    fit::result start_args = fidl::StandaloneDecode<fuchsia_driver_framework::DriverStartArgs>(
        fidl::EncodedMessage::FromEncodedCMessage(encoded_start_args.msg), wire_format_metadata);
    if (!start_args.is_ok()) {
      ZX_DEBUG_ASSERT_MSG(false, "Failed to decode start_args: %s",
                          start_args.error_value().FormatDescription().c_str());
      return start_args.error_value().status();
    }

    zx::result<std::unique_ptr<DriverBase>> created_driver = Factory::CreateDriver(
        std::move(*start_args), fdf::UnownedSynchronizedDispatcher(dispatcher));

    if (created_driver.is_error()) {
      return created_driver.status_value();
    }

    // Store `driver` pointer.
    *driver = (*created_driver).release();
    return ZX_OK;
  }

  static void PrepareStop(PrepareStopContext* context) {
    DriverBase* casted_driver = static_cast<DriverBase*>(context->driver);
    PrepareStopCompleter completer(context);
    casted_driver->PrepareStop(std::move(completer));
  }

  static zx_status_t Stop(void* driver) {
    // Make a unique_ptr take ownership of the driver. Once it goes out of scope at the end of this
    // function, the driver is deleted automatically.
    auto driver_ptr = std::unique_ptr<DriverBase>(static_cast<DriverBase*>(driver));
    driver_ptr->Stop();
    return ZX_OK;
  }
};

#define FUCHSIA_DRIVER_LIFECYCLE_CPP_V2(lifecycle) \
  FUCHSIA_DRIVER_LIFECYCLE_V1(.start = lifecycle::Start, .stop = lifecycle::Stop)

#define FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(lifecycle)                                               \
  FUCHSIA_DRIVER_LIFECYCLE_V2(.start = lifecycle::Start, .prepare_stop = lifecycle::PrepareStop, \
                              .stop = lifecycle::Stop)

}  // namespace fdf

#endif  // LIB_DRIVER_COMPONENT_CPP_LIFECYCLE_H_
