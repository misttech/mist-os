// Copyright 2021 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include <fidl/fuchsia.driver.framework/cpp/wire.h>
#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/driver/logging/cpp/structured_logger.h>

namespace {

class PackagedDriver : public fdf::DriverBase {
 public:
  PackagedDriver(fdf::DriverStartArgs start_args,
                 fdf::UnownedSynchronizedDispatcher driver_dispatcher)
      : fdf::DriverBase("packaged", std::move(start_args), std::move(driver_dispatcher)) {}

  zx::result<> Start() override {
    inspector().root().RecordString("hello", "world");

    FDF_SLOG(DEBUG, "Debug world");
    FDF_SLOG(INFO, "Hello world", KV("The answer is", 42));
    return zx::ok();
  }
};

}  // namespace

FUCHSIA_DRIVER_EXPORT(PackagedDriver);
