// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#include "src/devices/sysmem/drivers/sysmem/deprecated_device.h"

#include <lib/driver/component/cpp/driver_export.h>

DeprecatedDevice::DeprecatedDevice(fdf::DriverStartArgs start_args,
                                   fdf::UnownedSynchronizedDispatcher incoming_driver_dispatcher)
    : fdf::DriverBase("sysmem-device", std::move(start_args),
                      std::move(incoming_driver_dispatcher)) {
  // This whole class will be removed soon.
  FDF_LOG(INFO, "DeprecatedDevice::DeprecatedDevice");
}

zx::result<> DeprecatedDevice::Start() { return zx::ok(); }

FUCHSIA_DRIVER_EXPORT(DeprecatedDevice);
