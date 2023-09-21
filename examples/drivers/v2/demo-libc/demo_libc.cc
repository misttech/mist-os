// Copyright 2023 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>

namespace demo_libc {

// This class represents the driver instance.
class DemoLibc : public fdf::DriverBase {
 public:
  DemoLibc(fdf::DriverStartArgs start_args, fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : DriverBase("demo_libc", std::move(start_args), std::move(driver_dispatcher)) {}

  // Called by the driver framework to initialize the driver instance.
  zx::result<> Start() override { return zx::ok(); }
};

}  // namespace demo_libc

FUCHSIA_DRIVER_EXPORT(demo_libc::DemoLibc);
