// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be found in the LICENSE file.

#include "src/connectivity/wlan/drivers/lib/components/cpp/test/test_driver.h"

#include <lib/driver/component/cpp/driver_export.h>

namespace wlan::drivers::components::test {

TestDriver::TestDriver(fdf::DriverStartArgs start_args,
                       fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("netdev-test-driver", std::move(start_args), std::move(driver_dispatcher)) {}

void TestDriver::Start(fdf::StartCompleter completer) { completer(zx::ok()); }

void TestDriver::PrepareStop(fdf::PrepareStopCompleter completer) {
  if (stop_handler_) {
    stop_handler_->PrepareStop(std::move(completer));
    return;
  }
  completer(zx::ok());
}

void TestDriver::Stop() {
  if (stop_handler_) {
    stop_handler_->Stop();
  }
}

}  // namespace wlan::drivers::components::test

FUCHSIA_DRIVER_EXPORT(wlan::drivers::components::test::TestDriver);
