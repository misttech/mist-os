// Copyright 2018 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>

class Child3Driver : public fdf::DriverBase {
 public:
  Child3Driver(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("child-3", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override {
    std::vector<fuchsia_driver_framework::NodeProperty2> properties = {};
    zx::result result = AddChild("child-3", properties, {});
    if (result.is_error()) {
      return result.take_error();
    }
    return zx::ok();
  }
};

FUCHSIA_DRIVER_EXPORT(Child3Driver);
