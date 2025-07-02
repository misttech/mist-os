// Copyright 2025 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "tools/create/goldens/my-driver-cpp/my_driver_cpp.h"

#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/logger.h>

namespace my_driver_cpp {

MyDriverCpp::MyDriverCpp(fdf::DriverStartArgs start_args,
                                 fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("my_driver_cpp", std::move(start_args), std::move(driver_dispatcher)) {}

zx::result<> MyDriverCpp::Start() {
  return zx::ok();
}

void MyDriverCpp::PrepareStop(fdf::PrepareStopCompleter completer) {
  completer(zx::ok());
}

}  // namespace my_driver_cpp

FUCHSIA_DRIVER_EXPORT(my_driver_cpp::MyDriverCpp);
