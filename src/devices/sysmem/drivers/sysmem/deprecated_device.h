// Copyright 2024 The Fuchsia Authors. All rights reserved.
// Use of this source code is governed by a BSD-style license that can be
// found in the LICENSE file.

#ifndef SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_DEPRECATED_DEVICE_H_
#define SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_DEPRECATED_DEVICE_H_

#include <lib/driver/component/cpp/driver_base.h>

// This class exists as a stand-in for the former sysmem driver until CLs elsewhere stop expecting
// sysmem to be a driver. All the actual serving happens from sysmem_connector (name likely to
// change).
//
// TBD whether other repos can be ok without any sysmem driver loading (without needing this to
// exist temporarily).
class DeprecatedDevice final : public fdf::DriverBase {
 public:
  DeprecatedDevice(fdf::DriverStartArgs start_args,
                   fdf::UnownedSynchronizedDispatcher incoming_driver_dispatcher);
  zx::result<> Start() override;
};

#endif  // SRC_DEVICES_SYSMEM_DRIVERS_SYSMEM_DEPRECATED_DEVICE_H_
