// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "skeleton_driver.h"

#include <lib/driver/component/cpp/driver_export.h>

namespace skeleton {

SkeletonDriver::SkeletonDriver(fdf::DriverStartArgs start_args,
                               fdf::UnownedSynchronizedDispatcher driver_dispatcher)
    : DriverBase("skeleton_driver", std::move(start_args), std::move(driver_dispatcher)) {}

zx::result<> SkeletonDriver::Start() {
  // Instructions: Put driver initialization logic in this function, such as adding children
  // and setting up client-server transport connections.
  // If the initialization logic is asynchronous, prefer to override
  // DriverBase::Start(fdf::StartCompleter completer) over this function.
  return zx::ok();
}

}  // namespace skeleton

FUCHSIA_DRIVER_EXPORT(skeleton::SkeletonDriver);
