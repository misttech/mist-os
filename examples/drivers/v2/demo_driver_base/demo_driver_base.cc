// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/driver_cpp.h>

using fdf::DriverBase;
using fdf::DriverStartArgs;
using fdf::Lifecycle;

// ----------------------------------Default BasicFactory------------------------------------------
class MyDriver : public DriverBase {
 public:
  MyDriver(DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("my_driver", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override {
    // incoming()->Connect(...);
    // outgoing()->AddService(...);
    FDF_LOG(INFO, "hello world!");
    return zx::ok();
  }
};

// If we don't need a custom factory (default is the BasicFactory in driver_base.h) we can just
// put in this macro and stop.
// FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(Lifecycle<MyDriver>);

// ------------------------------------------------------------------------------------------------

// ------------------------------------Custom Factory----------------------------------------------

// If we have another driver that does not have the two argument constructor
// required by the |BasicFactory|, or if it just needs more complex logic to initialize it like
// custom constructor args, other init methods called, etc.. then we need a custom factory as well.
class AnotherDriver : public DriverBase {
 public:
  // Brings in the 3 argument constructor from |DriverBase|.
  // We don't need to provide the two arg constructor because our custom factory is
  // now the one who calls the constructor here, and it provides the name argument as well.
  using DriverBase::DriverBase;

  zx::result<> Start() override {
    // incoming()->Connect(...);
    // outgoing()->AddService(...);
    FDF_LOG(INFO, "foobar!");
    return zx::ok();
  }
};

// Here is our custom factory, we can pass it into our Lifecycle down below.
class CustomFactory {
 public:
  static void CreateDriver(DriverStartArgs start_args,
                           fdf::UnownedSynchronizedDispatcher driver_dispatcher,
                           fdf::StartCompleter completer) {
    // The logic here right now is similar to the one in |BasicFactory| but it does not have to be.
    // The driver author can run any custom constructor/initialization here.
    auto driver = std::make_unique<AnotherDriver>(
        std::string_view("custom_driver"), std::move(start_args), std::move(driver_dispatcher));
    DriverBase* driver_ptr = driver.get();
    completer.set_driver(std::move(driver));
    driver_ptr->Start(std::move(completer));
  }
};

// We must define the lifecycle before passing into the macro, otherwise the macro expansion
// will think the comma is to pass a second macro argument.
using lifecycle = Lifecycle<AnotherDriver, CustomFactory>;
FUCHSIA_DRIVER_LIFECYCLE_CPP_V3(lifecycle);
// ------------------------------------------------------------------------------------------------
