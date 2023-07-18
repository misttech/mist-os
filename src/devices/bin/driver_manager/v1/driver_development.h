// Copyright 2022 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_BIN_DRIVER_MANAGER_V1_DRIVER_DEVELOPMENT_H_
#define SRC_DEVICES_BIN_DRIVER_MANAGER_V1_DRIVER_DEVELOPMENT_H_

#include <fidl/fuchsia.driver.development/cpp/wire.h>
#include <lib/stdcompat/span.h>

#include "src/devices/bin/driver_manager/v1/device.h"

zx::result<std::vector<fuchsia_driver_development::wire::DeviceInfo>> GetDeviceInfo(
    fidl::AnyArena& allocator, const std::vector<fbl::RefPtr<const Device>>& devices);

#endif  // SRC_DEVICES_BIN_DRIVER_MANAGER_V1_DRIVER_DEVELOPMENT_H_
