// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/graphics/display/lib/driver-framework-migration-utils/logging/testing/dfv2-driver-with-logging.h"

#include <lib/driver/component/cpp/driver_base.h>
#include <lib/driver/component/cpp/driver_export.h>
#include <lib/zx/result.h>

namespace display::testing {

Dfv2DriverWithLogging::Dfv2DriverWithLogging(fdf::DriverStartArgs start_args,
                                             fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : fdf::DriverBase("dfv2-driver-with-logging", std::move(start_args),
                      std::move(driver_dispatcher)) {}
Dfv2DriverWithLogging::~Dfv2DriverWithLogging() = default;

zx::result<> Dfv2DriverWithLogging::Start() { return zx::ok(); }

bool Dfv2DriverWithLogging::LogTrace() const { return logging_hardware_module_.LogTrace(); }

bool Dfv2DriverWithLogging::LogDebug() const { return logging_hardware_module_.LogDebug(); }

bool Dfv2DriverWithLogging::LogInfo() const { return logging_hardware_module_.LogInfo(); }

bool Dfv2DriverWithLogging::LogWarning() const { return logging_hardware_module_.LogWarning(); }

bool Dfv2DriverWithLogging::LogError() const { return logging_hardware_module_.LogError(); }

}  // namespace display::testing

FUCHSIA_DRIVER_EXPORT(::display::testing::Dfv2DriverWithLogging);
