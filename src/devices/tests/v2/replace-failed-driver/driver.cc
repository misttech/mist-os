// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <lib/driver/component/cpp/driver_export.h>
#include <zircon/errors.h>

namespace {

class FailerDriver : public fdf::DriverBase {
 public:
  FailerDriver(fdf::DriverStartArgs start_args,
               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase("failer", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override {
    FDF_LOG(INFO, "This driver is returning a ZX_ERR_NOT_SUPPORTED error.");
    return zx::error(ZX_ERR_NOT_SUPPORTED);
  }
};

}  // namespace

FUCHSIA_DRIVER_EXPORT(FailerDriver);
