// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>

using fdf::DriverBase;
using fdf::DriverStartArgs;

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

FUCHSIA_DRIVER_EXPORT(MyDriver);
