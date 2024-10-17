// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/driver_export.h>
#include <zircon/errors.h>

namespace {

class FailerReplacementDriver : public fdf::DriverBase {
 public:
  FailerReplacementDriver(fdf::DriverStartArgs start_args,
                          fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase("failer-replacement", std::move(start_args), std::move(driver_dispatcher)) {
  }

  zx::result<> Start() override {
    FDF_LOG(INFO, "This driver is returning OK.");
    return zx::ok();
  }
};

}  // namespace

FUCHSIA_DRIVER_EXPORT(FailerReplacementDriver);
